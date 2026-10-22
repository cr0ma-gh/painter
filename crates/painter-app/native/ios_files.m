// iPadOS document picker for Painter (see src/files.rs).
//
// Opening: UIDocumentPickerViewController in copy mode, so the app gets its
// own copies of the chosen files (from Files, iCloud Drive, other apps); their
// paths queue up here and Rust takes them one at a time each frame.
// Exporting: the same picker in export mode puts a copy of a file Painter
// wrote wherever the user chooses.

#import <UIKit/UIKit.h>
#import <UniformTypeIdentifiers/UniformTypeIdentifiers.h>
#include <stdlib.h>
#include <string.h>

static NSMutableArray<NSString *> *picked;
static volatile int picker_active;

@interface PainterPickerDelegate : NSObject <UIDocumentPickerDelegate>
@end

@implementation PainterPickerDelegate
- (void)documentPicker:(UIDocumentPickerViewController *)controller didPickDocumentsAtURLs:(NSArray<NSURL *> *)urls {
    @synchronized([PainterPickerDelegate class]) {
        if (!picked) picked = [NSMutableArray array];
        if (controller.documentPickerMode != UIDocumentPickerModeExportToService) {
            for (NSURL *url in urls) {
                if (url.isFileURL) [picked addObject:url.path];
            }
        }
    }
    picker_active = 0;
}

- (void)documentPickerWasCancelled:(UIDocumentPickerViewController *)controller {
    picker_active = 0;
}
@end

// The picker holds its delegate weakly.
static PainterPickerDelegate *delegate;

UIViewController *painter_top_view_controller(void) {
    UIWindow *window = nil;
    for (UIScene *scene in UIApplication.sharedApplication.connectedScenes) {
        if (![scene isKindOfClass:[UIWindowScene class]]) continue;
        for (UIWindow *w in ((UIWindowScene *)scene).windows) {
            if (w.isKeyWindow) { window = w; break; }
            if (!window) window = w;
        }
        if (window.isKeyWindow) break;
    }
    UIViewController *vc = window.rootViewController;
    while (vc.presentedViewController) vc = vc.presentedViewController;
    return vc;
}

static void present(UIDocumentPickerViewController *picker) {
    if (!delegate) delegate = [PainterPickerDelegate new];
    picker.delegate = delegate;
    picker.modalPresentationStyle = UIModalPresentationFormSheet;
    UIViewController *vc = painter_top_view_controller();
    if (!vc) { picker_active = 0; return; }
    [vc presentViewController:picker animated:YES completion:nil];
}

void painter_pick_documents(int multiple) {
    picker_active = 1;
    dispatch_async(dispatch_get_main_queue(), ^{
        UIDocumentPickerViewController *picker =
            [[UIDocumentPickerViewController alloc] initForOpeningContentTypes:@[ UTTypeItem ] asCopy:YES];
        picker.allowsMultipleSelection = multiple != 0;
        present(picker);
    });
}

void painter_export_document(const char *path) {
    if (!path) return;
    NSURL *url = [NSURL fileURLWithPath:[NSString stringWithUTF8String:path]];
    picker_active = 1;
    dispatch_async(dispatch_get_main_queue(), ^{
        UIDocumentPickerViewController *picker =
            [[UIDocumentPickerViewController alloc] initForExportingURLs:@[ url ] asCopy:YES];
        present(picker);
    });
}

int painter_picker_active(void) { return picker_active; }

// The oldest picked path (malloc'd; free with painter_free_string), or NULL.
char *painter_take_picked(void) {
    @synchronized([PainterPickerDelegate class]) {
        if (picked.count == 0) return NULL;
        NSString *path = picked.firstObject;
        [picked removeObjectAtIndex:0];
        return strdup(path.fileSystemRepresentation);
    }
}

void painter_free_string(char *s) { free(s); }

// The picture on the system pasteboard as PNG (malloc'd, `*len` bytes; free
// with painter_free_string), or NULL when there's none.
char *painter_clipboard_png(size_t *len) {
    __block NSData *png = nil;
    void (^read)(void) = ^{
        UIImage *image = UIPasteboard.generalPasteboard.image;
        if (!image) return;
        // Drawn again at one pixel per pixel, upright.
        CGSize size = CGSizeMake(image.size.width * image.scale, image.size.height * image.scale);
        UIGraphicsImageRendererFormat *format = [UIGraphicsImageRendererFormat preferredFormat];
        format.scale = 1;
        format.opaque = NO;
        UIGraphicsImageRenderer *renderer = [[UIGraphicsImageRenderer alloc] initWithSize:size format:format];
        png = [renderer PNGDataWithActions:^(UIGraphicsImageRendererContext *ctx) {
            [image drawInRect:CGRectMake(0, 0, size.width, size.height)];
        }];
    };
    if (NSThread.isMainThread) read(); else dispatch_sync(dispatch_get_main_queue(), read);
    if (!png || png.length == 0) return NULL;
    char *out = malloc(png.length);
    if (!out) return NULL;
    memcpy(out, png.bytes, png.length);
    *len = png.length;
    return out;
}
