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
