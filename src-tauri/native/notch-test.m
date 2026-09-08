// Compile separately on an unlocked Mac. Includes the production implementation
// so fixtures exercise the actual panel, paths, state rendering, and callbacks.
#import "notch.m"
#include <assert.h>
#include <stdio.h>

static int lastAction = -1;
static void action(int value) { lastAction = value; }
static void pump(double seconds) {
    NSDate *end = [NSDate dateWithTimeIntervalSinceNow:seconds];
    while (end.timeIntervalSinceNow > 0) {
        [NSRunLoop.mainRunLoop runMode:NSDefaultRunLoopMode beforeDate:[NSDate dateWithTimeIntervalSinceNow:.01]];
    }
}
static void snapshot(NSString *directory, NSString *name) {
    [controller.view layoutSubtreeIfNeeded];
    [controller.view displayIfNeeded];
    for (NSView *view in controller.view.subviews) {
        if ([view isKindOfClass:NSTextField.class]) assert(view == controller.view.clock);
    }
    NSBitmapImageRep *rep = [controller.view bitmapImageRepForCachingDisplayInRect:controller.view.bounds];
    [controller.view cacheDisplayInRect:controller.view.bounds toBitmapImageRep:rep];
    NSData *png = [rep representationUsingType:NSBitmapImageFileTypePNG properties:@{}];
    [png writeToFile:[directory stringByAppendingPathComponent:[name stringByAppendingString:@".png"]] atomically:YES];
}
static void placementTests(void) {
    for (NSValue *value in @[[NSValue valueWithRect:NSMakeRect(0,0,1512,982)],
                             [NSValue valueWithRect:NSMakeRect(-1920,-200,1920,1080)],
                             [NSValue valueWithRect:NSMakeRect(100,982,2560,1440)]]) {
        NSRect screen = value.rectValue;
        NSRect visible = NSInsetRect(screen,0,24);
        for (NSNumber *safe in @[@0,@32,@38]) {
            NSRect panel = panelFrame(screen,visible,safe.doubleValue,180);
            assert(fabs(NSMidX(panel)-NSMidX(screen)) < .01);
            assert(NSMinX(panel) >= NSMinX(screen));
            assert(NSMaxX(panel) <= NSMaxX(screen));
            assert(NSMinY(panel) >= NSMinY(visible));
            assert(fabs(NSMaxY(panel) - (safe.doubleValue > 0 ? NSMaxY(screen) : NSMaxY(visible)-12)) < .01);
            CGPathRef path = surfacePath(NSMakeRect(0,0,panel.size.width,panel.size.height), safe.doubleValue, 180);
            assert(CGPathContainsPoint(path,NULL,CGPointMake(panel.size.width/2,25),false));
            assert(!CGPathContainsPoint(path,NULL,CGPointMake(0,0),false));
            if (safe.doubleValue > 0) {
                assert(CGPathContainsPoint(path,NULL,CGPointMake(panel.size.width/2,panel.size.height-1),false));
                assert(CGPathContainsPoint(path,NULL,CGPointMake(20,panel.size.height-1),false));
            }
            CGPathRelease(path);
        }
    }
}
int main(int argc, const char *argv[]) {
    @autoreleasepool {
        [NSApplication sharedApplication];
        [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
        [NSApp finishLaunching];
        placementTests();
        pump(.5); // AppKit finishes application registration before measuring focus.
        pid_t foreground = NSWorkspace.sharedWorkspace.frontmostApplication.processIdentifier;
        printf("Initial foreground %d (%s), fixture %d\n", foreground, NSWorkspace.sharedWorkspace.frontmostApplication.localizedName.UTF8String, NSProcessInfo.processInfo.processIdentifier);
        tc_notch_init(action);
        assert(!controller.panel.canBecomeKeyWindow && !controller.panel.canBecomeMainWindow);
        assert(controller.panel.styleMask & NSWindowStyleMaskNonactivatingPanel);
        assert(controller.panel.collectionBehavior & NSWindowCollectionBehaviorFullScreenAuxiliary);
        for (NSScreen *screen in NSScreen.screens) {
            printf("Display %.0fx%.0f at %.0f,%.0f scale %.1f safeTop %.0f notch %.0f\n", screen.frame.size.width,screen.frame.size.height,screen.frame.origin.x,screen.frame.origin.y,screen.backingScaleFactor,screen.safeAreaInsets.top,
                NSMinX(screen.auxiliaryTopRightArea)-NSMaxX(screen.auxiliaryTopLeftArea));
        }
        NSString *directory = argc > 1 ? [NSString stringWithUTF8String:argv[1]] : @"/tmp/transcribe-notch-qa";
        [NSFileManager.defaultManager createDirectoryAtPath:directory withIntermediateDirectories:YES attributes:nil error:nil];
        int64_t startedAt = (int64_t)(NSDate.date.timeIntervalSince1970*1000)-12500;
        tc_notch_update("starting",0,NULL);
        if (!controller.view.reducedMotion && controller.view.safeTop > 0) {
            pump(.08);
            CAShapeLayer *mask = (CAShapeLayer *)controller.view.layer.mask;
            CAShapeLayer *presentation = (CAShapeLayer *)mask.presentationLayer;
            assert(presentation.path);
            CGFloat width = CGPathGetBoundingBox(presentation.path).size.width;
            assert(width > controller.view.notchWidth);
            assert(width < NSWidth(controller.view.bounds)-2*gutter+10);
        }
        pump(.4);
        assert(controller.panel.visible);
        snapshot(directory,@"starting");
        tc_notch_update("recording",startedAt,NULL); tc_notch_level(.11); pump(.3);
        assert(controller.view.smoothedLevel > .3);
        for (NSView *view in controller.view.subviews) assert(![view isKindOfClass:NSButton.class]);
        assert(lastAction == -1);
        assert([controller.view.clock.stringValue isEqualToString:@"00:12"] || [controller.view.clock.stringValue isEqualToString:@"00:13"]);
        printf("Current foreground %d (%s)\n", NSWorkspace.sharedWorkspace.frontmostApplication.processIdentifier, NSWorkspace.sharedWorkspace.frontmostApplication.localizedName.UTF8String);
        assert(NSWorkspace.sharedWorkspace.frontmostApplication.processIdentifier == foreground);
        [controller.view layoutSubtreeIfNeeded];
        assert(CGColorEqualToColor(controller.view.surface.layer.backgroundColor, NSColor.blackColor.CGColor));
        assert(CGColorEqualToColor(controller.view.layer.backgroundColor, NSColor.blackColor.CGColor));
        assert(controller.view.layer.opacity == 1 && controller.panel.alphaValue == 1);
        if (controller.view.safeTop > 0) {
            NSRect bounds = controller.view.bounds;
            CGFloat notchLeft = NSMidX(bounds)-controller.view.notchWidth/2;
            CGFloat notchRight = NSMidX(bounds)+controller.view.notchWidth/2;
            assert(NSMaxX(controller.view.waveform.frame) <= notchLeft);
            assert(NSMinX(controller.view.clock.frame) >= notchRight);
            assert(fabs(NSHeight(bounds)-gutter-controller.view.safeTop) < .01);
        }
        snapshot(directory,@"recording");
        // Synthetic notch on the available display, for deterministic visual QA
        // even when running with the laptop closed or external screens only.
        controller.view.safeTop = 32; controller.view.notchWidth = 180;
        [controller.panel setContentSize:NSMakeSize(180+104+2*gutter,32+gutter)];
        [controller.view setNeedsLayout:YES];
        snapshot(directory,@"recording-notch");
        tc_notch_level(NAN); [controller.view tick];
        assert(isfinite(controller.view.smoothedLevel));
        tc_notch_update("transcribing",startedAt,NULL); pump(.15);
        NSString *elapsed = controller.view.clock.stringValue;
        pump(1.1); assert([controller.view.clock.stringValue isEqualToString:elapsed]);
        assert(![controller.view.layer.mask animationForKey:@"expansion"]);
        snapshot(directory,@"transcribing");
        tc_notch_update("inserting",startedAt,NULL); pump(.1);
        snapshot(directory,@"inserting");
        tc_notch_update("done",startedAt,NULL); pump(.1);
        pump(.3);
        assert(!controller.panel.visible && !controller.requested);
        tc_notch_update("error",startedAt,"Destination changed. Your transcript is saved in History."); pump(.1);
        assert([controller.view.accessibilityLabel containsString:@"Destination changed"]);
        assert(controller.view.waveform.hidden && controller.view.clock.hidden);
        assert(!controller.view.errorIcon.hidden);
        snapshot(directory,@"error");
        [controller.panel orderOut:nil]; assert(!controller.panel.visible);
        [controller position]; assert(controller.panel.visible);
        tc_notch_hide(); tc_notch_update("recording",startedAt,NULL); pump(.35);
        assert(controller.panel.visible && controller.panel.alphaValue > .99);
        assert(!controller.view.waveform.hidden && !controller.view.clock.hidden);
        assert(controller.view.errorIcon.hidden);
        tc_notch_hide(); pump(.3);
        assert(!controller.panel.visible && !controller.timer);
        [controller position]; assert(!controller.panel.visible);
        printf("Current foreground %d (%s)\n", NSWorkspace.sharedWorkspace.frontmostApplication.processIdentifier, NSWorkspace.sharedWorkspace.frontmostApplication.localizedName.UTF8String);
        assert(NSWorkspace.sharedWorkspace.frontmostApplication.processIdentifier == foreground);
        tc_notch_update("recording",startedAt,NULL);
        controller.view.reducedMotion = YES;
        CGFloat motionTime = controller.view.motionTime;
        [controller.view tick]; assert(controller.view.motionTime == motionTime);
        tc_notch_hide(); assert(!controller.panel.visible);
        tc_notch_destroy(); assert(!controller);
        puts("PASS: geometry, states, opaque background, no controls, audio, timer, focus, visibility recovery, and rapid restart");
        return 0;
    }
}
