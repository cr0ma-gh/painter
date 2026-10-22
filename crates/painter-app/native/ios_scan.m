// iPadOS document camera for File → Import → From Scanner (see src/scan_source.rs).
//
// VNDocumentCameraViewController finds the sheet in the camera image,
// straightens its perspective and can take several pages. Each page is
// written as a PNG to the temporary folder; the paths queue up here and Rust
// takes them once the camera closes.

#import <UIKit/UIKit.h>
#import <VisionKit/VisionKit.h>
#include <stdlib.h>
#include <string.h>

UIViewController *painter_top_view_controller(void);

static NSMutableArray<NSString *> *scanned;
static NSString *scan_error;
static volatile int scan_active;

@interface PainterScanDelegate : NSObject <VNDocumentCameraViewControllerDelegate>
@end

@implementation PainterScanDelegate
- (void)documentCameraViewController:(VNDocumentCameraViewController *)controller didFinishWithScan:(VNDocumentCameraScan *)scan {
    NSString *dir = NSTemporaryDirectory();
    NSString *stamp = [[NSUUID UUID] UUIDString];
    NSMutableArray<NSString *> *pages = [NSMutableArray array];
    for (NSUInteger i = 0; i < scan.pageCount; i++) {
        UIImage *page = [scan imageOfPageAtIndex:i];
        // Drawn again at one pixel per pixel, so the PNG is upright whatever
        // the image's orientation flag says.
        CGSize size = CGSizeMake(page.size.width * page.scale, page.size.height * page.scale);
        UIGraphicsImageRendererFormat *format = [UIGraphicsImageRendererFormat preferredFormat];
        format.scale = 1;
        format.opaque = YES;
        UIGraphicsImageRenderer *renderer = [[UIGraphicsImageRenderer alloc] initWithSize:size format:format];
        NSData *png = [renderer PNGDataWithActions:^(UIGraphicsImageRendererContext *ctx) {
            [page drawInRect:CGRectMake(0, 0, size.width, size.height)];
        }];
        NSString *path = [dir stringByAppendingPathComponent:[NSString stringWithFormat:@"painter-scan-%@-%lu.png", stamp, (unsigned long)i + 1]];
        if ([png writeToFile:path atomically:YES]) [pages addObject:path];
    }
    @synchronized([PainterScanDelegate class]) {
        if (!scanned) scanned = [NSMutableArray array];
        [scanned addObjectsFromArray:pages];
    }
    [controller dismissViewControllerAnimated:YES completion:nil];
    scan_active = 0;
}

- (void)documentCameraViewControllerDidCancel:(VNDocumentCameraViewController *)controller {
    [controller dismissViewControllerAnimated:YES completion:nil];
    scan_active = 0;
}

- (void)documentCameraViewController:(VNDocumentCameraViewController *)controller didFailWithError:(NSError *)error {
    @synchronized([PainterScanDelegate class]) {
        scan_error = error.localizedDescription;
    }
    [controller dismissViewControllerAnimated:YES completion:nil];
    scan_active = 0;
}
@end

// The camera holds its delegate weakly.
static PainterScanDelegate *scan_delegate;

int painter_scan_supported(void) { return VNDocumentCameraViewController.isSupported ? 1 : 0; }

void painter_scan_document(void) {
    scan_active = 1;
    dispatch_async(dispatch_get_main_queue(), ^{
        if (!VNDocumentCameraViewController.isSupported) {
            @synchronized([PainterScanDelegate class]) {
                scan_error = @"This iPad has no camera for scanning";
            }
            scan_active = 0;
            return;
        }
        if (!scan_delegate) scan_delegate = [PainterScanDelegate new];
        VNDocumentCameraViewController *camera = [VNDocumentCameraViewController new];
        camera.delegate = scan_delegate;
        UIViewController *vc = painter_top_view_controller();
        if (!vc) {
            scan_active = 0;
            return;
        }
        [vc presentViewController:camera animated:YES completion:nil];
    });
}

int painter_scan_active(void) { return scan_active; }

// The oldest scanned page's path (malloc'd; free with painter_free_string), or NULL.
char *painter_take_scanned(void) {
    @synchronized([PainterScanDelegate class]) {
        if (scanned.count == 0) return NULL;
        NSString *path = scanned.firstObject;
        [scanned removeObjectAtIndex:0];
        return strdup(path.fileSystemRepresentation);
    }
}

// Why the last scan failed (malloc'd), or NULL.
char *painter_take_scan_error(void) {
    @synchronized([PainterScanDelegate class]) {
        if (!scan_error) return NULL;
        char *s = strdup(scan_error.UTF8String);
        scan_error = nil;
        return s;
    }
}

// ---- network scanners (eSCL / AirScan) through Bonjour ---------------------------
//
// iPadOS doesn't let apps send multicast themselves; NSNetServiceBrowser asks
// mDNSResponder for "_uscan._tcp" instead (Info.plist: NSBonjourServices and
// NSLocalNetworkUsageDescription). Each scanner found is resolved to an IPv4
// address and port and queued as "name\turl" for Rust.

#include <arpa/inet.h>

static NSMutableArray<NSString *> *network_found;

@interface PainterScannerBrowser : NSObject <NSNetServiceBrowserDelegate, NSNetServiceDelegate>
@property (strong) NSNetServiceBrowser *browser;
@property (strong) NSMutableArray<NSNetService *> *resolving;
@end

@implementation PainterScannerBrowser
- (void)netServiceBrowser:(NSNetServiceBrowser *)browser didFindService:(NSNetService *)service moreComing:(BOOL)more {
    [self.resolving addObject:service];
    service.delegate = self;
    [service resolveWithTimeout:5];
}

- (void)netServiceDidResolveAddress:(NSNetService *)service {
    NSString *ip = nil;
    for (NSData *data in service.addresses) {
        const struct sockaddr *sa = data.bytes;
        if (data.length >= sizeof(struct sockaddr_in) && sa->sa_family == AF_INET) {
            char buf[INET_ADDRSTRLEN];
            const struct sockaddr_in *in = (const struct sockaddr_in *)sa;
            if (inet_ntop(AF_INET, &in->sin_addr, buf, sizeof buf)) ip = [NSString stringWithUTF8String:buf];
            break;
        }
    }
    if (!ip) ip = service.hostName;
    NSString *rs = @"eSCL";
    NSString *name = service.name;
    NSData *txt = service.TXTRecordData;
    if (txt) {
        NSDictionary<NSString *, NSData *> *dict = [NSNetService dictionaryFromTXTRecordData:txt];
        NSData *r = dict[@"rs"];
        if (r.length) rs = [[NSString alloc] initWithData:r encoding:NSUTF8StringEncoding] ?: rs;
        NSData *ty = dict[@"ty"];
        if (ty.length) name = [[NSString alloc] initWithData:ty encoding:NSUTF8StringEncoding] ?: name;
    }
    rs = [rs stringByTrimmingCharactersInSet:[NSCharacterSet characterSetWithCharactersInString:@"/"]];
    NSString *line = [NSString stringWithFormat:@"%@\thttp://%@:%ld/%@", name, ip, (long)service.port, rs];
    @synchronized([PainterScannerBrowser class]) {
        if (!network_found) network_found = [NSMutableArray array];
        [network_found addObject:line];
    }
    [self.resolving removeObject:service];
}

- (void)netService:(NSNetService *)service didNotResolve:(NSDictionary *)errorDict {
    [self.resolving removeObject:service];
}
@end

static PainterScannerBrowser *scanner_browser;

void painter_find_network_scanners(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        if (scanner_browser) [scanner_browser.browser stop];
        scanner_browser = [PainterScannerBrowser new];
        scanner_browser.resolving = [NSMutableArray array];
        scanner_browser.browser = [NSNetServiceBrowser new];
        scanner_browser.browser.delegate = scanner_browser;
        [scanner_browser.browser searchForServicesOfType:@"_uscan._tcp." inDomain:@"local."];
    });
}

// The oldest scanner found ("name\turl", malloc'd), or NULL.
char *painter_take_network_scanner(void) {
    @synchronized([PainterScannerBrowser class]) {
        if (network_found.count == 0) return NULL;
        NSString *line = network_found.firstObject;
        [network_found removeObjectAtIndex:0];
        return strdup(line.UTF8String);
    }
}
