// iPadOS window behaviour for Painter (see src/ios_ui.rs).
//
// winit's view controller (WinitUIViewController) shows the status bar and
// lets the system take every edge swipe, which puts iOS's clock / battery bar
// and the Notification / Control Center pull-downs on top of Painter's menu
// bar. eframe gives no access to winit's own settings for these, so the
// controller's answers are replaced here: status bar hidden, home indicator
// dimmed, and all edges deferred (the first swipe from an edge goes to the
// app; a second one still opens the system UI).

#import <UIKit/UIKit.h>
#import <objc/runtime.h>
#include <stdio.h>

static UIWindow *painter_window(void) {
    UIWindow *window = nil;
    for (UIScene *scene in UIApplication.sharedApplication.connectedScenes) {
        if (![scene isKindOfClass:[UIWindowScene class]]) continue;
        for (UIWindow *w in ((UIWindowScene *)scene).windows) {
            if (w.isKeyWindow) return w;
            if (!window) window = w;
        }
    }
    return window;
}

static void replace(Class cls, SEL sel, id block, const char *types) {
    IMP imp = imp_implementationWithBlock(block);
    if (!class_addMethod(cls, sel, imp, types)) class_replaceMethod(cls, sel, imp, types);
}

// 1 once the window exists and has been set up; call again until then.
int painter_ios_prepare_window(void) {
    if (![NSThread isMainThread]) return 0;
    UIViewController *vc = painter_window().rootViewController;
    if (!vc) return 0;
    Class cls = object_getClass(vc);
    replace(cls, @selector(prefersStatusBarHidden), ^BOOL(id self) { return YES; }, "B@:");
    replace(cls, @selector(prefersHomeIndicatorAutoHidden), ^BOOL(id self) { return YES; }, "B@:");
    char edges[16];
    snprintf(edges, sizeof edges, "%s@:", @encode(UIRectEdge));
    replace(cls, @selector(preferredScreenEdgesDeferringSystemGestures), ^UIRectEdge(id self) { return UIRectEdgeAll; }, edges);
    [vc setNeedsStatusBarAppearanceUpdate];
    [vc setNeedsUpdateOfHomeIndicatorAutoHidden];
    [vc setNeedsUpdateOfScreenEdgesDeferringSystemGestures];
    return 1;
}

// The window's safe area insets in points (rounded corners, camera housing).
void painter_ios_safe_area(double *top, double *left, double *bottom, double *right) {
    *top = *left = *bottom = *right = 0;
    if (![NSThread isMainThread]) return;
    UIWindow *window = painter_window();
    if (!window) return;
    UIEdgeInsets insets = window.safeAreaInsets;
    *top = insets.top;
    *left = insets.left;
    *bottom = insets.bottom;
    *right = insets.right;
}

// ---- Apple Pencil tilt (see src/tablet.rs) ---------------------------------
//
// winit reports the Pencil's altitude, but egui keeps only the force; the
// azimuth isn't passed on at all. This recognizer watches every touch on the
// app's view (it never recognises, cancels or delays anything) and keeps the
// latest Pencil touch's altitude and azimuth for Rust to read each frame.

#import <UIKit/UIGestureRecognizerSubclass.h>

static double pencil_altitude = M_PI_2, pencil_azimuth = 0, pencil_time = 0;
static int pencil_seen = 0;

@interface PainterPencilObserver : UIGestureRecognizer <UIGestureRecognizerDelegate>
@end

@implementation PainterPencilObserver
- (void)record:(NSSet<UITouch *> *)touches {
    for (UITouch *touch in touches) {
        if (touch.type != UITouchTypePencil) continue;
        pencil_altitude = touch.altitudeAngle;
        pencil_azimuth = [touch azimuthAngleInView:self.view];
        pencil_time = NSProcessInfo.processInfo.systemUptime;
        pencil_seen = 1;
    }
}
- (void)touchesBegan:(NSSet<UITouch *> *)touches withEvent:(UIEvent *)event {
    [super touchesBegan:touches withEvent:event];
    [self record:touches];
}
- (void)touchesMoved:(NSSet<UITouch *> *)touches withEvent:(UIEvent *)event {
    [super touchesMoved:touches withEvent:event];
    [self record:touches];
}
- (void)touchesEstimatedPropertiesUpdated:(NSSet<UITouch *> *)touches {
    [self record:touches];
}
- (void)touchesEnded:(NSSet<UITouch *> *)touches withEvent:(UIEvent *)event {
    [super touchesEnded:touches withEvent:event];
    [self record:touches];
    self.state = UIGestureRecognizerStateFailed;
}
- (void)touchesCancelled:(NSSet<UITouch *> *)touches withEvent:(UIEvent *)event {
    [super touchesCancelled:touches withEvent:event];
    self.state = UIGestureRecognizerStateFailed;
}
- (BOOL)gestureRecognizer:(UIGestureRecognizer *)a shouldRecognizeSimultaneouslyWithGestureRecognizer:(UIGestureRecognizer *)b {
    return YES;
}
@end

// ---- Hover: the Pencil above the screen, a trackpad or mouse pointer --------
//
// winit reports nothing until something touches, so the brush cursor had
// nowhere to be on iPad. A hover recognizer sees the Apple Pencil hovering
// (iPad Pro M2 and later, with Pencil 2 or Pro; its tilt too from iPadOS
// 16.4) and the pointer of a trackpad or mouse. Each change asks the view to
// redraw, so the cursor follows even while nothing else happens.

static double hover_x = 0, hover_y = 0;
static int hover_active = 0;

@interface PainterHoverObserver : NSObject
@end

@implementation PainterHoverObserver
- (void)hover:(UIHoverGestureRecognizer *)g {
    UIView *view = g.view;
    CGPoint p = [g locationInView:view];
    CGFloat scale = view.contentScaleFactor;
    hover_x = p.x * scale;
    hover_y = p.y * scale;
    hover_active = (g.state == UIGestureRecognizerStateBegan || g.state == UIGestureRecognizerStateChanged);
    if (@available(iOS 16.4, *)) {
        if (hover_active) {
            pencil_altitude = g.altitudeAngle;
            pencil_azimuth = [g azimuthAngleInView:view];
            pencil_time = NSProcessInfo.processInfo.systemUptime;
            pencil_seen = 1;
        }
    }
    [view setNeedsDisplay];
}
@end

// 1 once the observers are on the app's view; call again until then.
int painter_pencil_install(void) {
    static PainterPencilObserver *observer = nil;
    static PainterHoverObserver *hover_target = nil;
    if (observer) return 1;
    if (![NSThread isMainThread]) return 0;
    UIView *view = painter_window().rootViewController.view;
    if (!view) return 0;
    observer = [[PainterPencilObserver alloc] initWithTarget:nil action:nil];
    observer.cancelsTouchesInView = NO;
    observer.delaysTouchesBegan = NO;
    observer.delaysTouchesEnded = NO;
    observer.delegate = observer;
    [view addGestureRecognizer:observer];
    hover_target = [[PainterHoverObserver alloc] init];
    UIHoverGestureRecognizer *hover = [[UIHoverGestureRecognizer alloc] initWithTarget:hover_target action:@selector(hover:)];
    [view addGestureRecognizer:hover];
    return 1;
}

// Where the Pencil hovers (or the pointer is), in the view's physical pixels; 0 when nothing hovers.
int painter_hover_state(double *x, double *y) {
    if (!hover_active) return 0;
    *x = hover_x;
    *y = hover_y;
    return 1;
}

// The latest Pencil touch: altitude (radians, π/2 = upright), azimuth in the
// view (radians clockwise from +x) and how many seconds ago. 0 before any.
int painter_pencil_state(double *altitude, double *azimuth, double *age) {
    if (!pencil_seen) return 0;
    *altitude = pencil_altitude;
    *azimuth = pencil_azimuth;
    *age = NSProcessInfo.processInfo.systemUptime - pencil_time;
    return 1;
}

// ---- Updates (see src/updater.rs) --------------------------------------------
//
// The updater asks GitHub for the latest release over HTTPS on a thread of
// its own (never the main thread: this waits for the answer), and opens the
// release page in Safari to sideload the new IPA.

#include <stdlib.h>
#include <string.h>

// The body of an HTTPS GET (status 200), malloc'd (free with painter_free), or NULL.
uint8_t *painter_https_get(const char *url, size_t *len) {
    NSURL *target = [NSURL URLWithString:[NSString stringWithUTF8String:url]];
    if (!target || [NSThread isMainThread]) return NULL;
    NSMutableURLRequest *request = [NSMutableURLRequest requestWithURL:target];
    [request setValue:@"Painter-updater" forHTTPHeaderField:@"User-Agent"];
    request.timeoutInterval = 20;
    __block NSData *body = nil;
    dispatch_semaphore_t done = dispatch_semaphore_create(0);
    NSURLSessionDataTask *task = [NSURLSession.sharedSession dataTaskWithRequest:request
                                                               completionHandler:^(NSData *data, NSURLResponse *response, NSError *error) {
        if (!error && [response isKindOfClass:[NSHTTPURLResponse class]] && ((NSHTTPURLResponse *)response).statusCode == 200) {
            body = data;
        }
        dispatch_semaphore_signal(done);
    }];
    [task resume];
    dispatch_semaphore_wait(done, DISPATCH_TIME_FOREVER);
    if (!body) return NULL;
    *len = body.length;
    uint8_t *out = malloc(body.length ? body.length : 1);
    if (out) memcpy(out, body.bytes, body.length);
    return out;
}

void painter_free(uint8_t *p) {
    free(p);
}

// Opens a link in Safari.
void painter_open_url(const char *url) {
    NSString *text = [NSString stringWithUTF8String:url];
    dispatch_async(dispatch_get_main_queue(), ^{
        NSURL *target = [NSURL URLWithString:text];
        if (target) [UIApplication.sharedApplication openURL:target options:@{} completionHandler:nil];
    });
}

// Battery charge 0..1 (-1 when unknown); *charging is 1 while plugged in.
double painter_battery_level(int *charging) {
    UIDevice *device = UIDevice.currentDevice;
    if (!device.batteryMonitoringEnabled) device.batteryMonitoringEnabled = YES;
    UIDeviceBatteryState state = device.batteryState;
    *charging = (state == UIDeviceBatteryStateCharging || state == UIDeviceBatteryStateFull) ? 1 : 0;
    return device.batteryLevel;
}
