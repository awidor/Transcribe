#import <AppKit/AppKit.h>
#import <QuartzCore/QuartzCore.h>
#import <ApplicationServices/ApplicationServices.h>
#import "notch.h"
#include <stdatomic.h>
#include <math.h>

static _Atomic(float) inputLevel;
static TCNotchAction actionHandler;
static const CGFloat gutter = 16;
static const CGFloat bodyHeight = 36;

// AppKit coordinates are points, with an upward Y axis, including on scaled
// and vertically arranged displays. Do not convert through the primary screen.
static NSRect panelFrame(NSRect screen, NSRect visible, CGFloat safeTop, CGFloat notchWidth) {
    CGFloat width = MIN(safeTop > 0 ? notchWidth + 180 : 260, NSWidth(visible) - 2 * gutter);
    CGFloat height = safeTop > 0 ? safeTop + gutter : bodyHeight + 2 * gutter;
    CGFloat top = safeTop > 0 ? NSMaxY(screen) : NSMaxY(visible) - 12;
    return NSMakeRect(NSMidX(screen) - width / 2 - gutter, top - height,
                      width + 2 * gutter, height);
}

@interface TCNotchPanel : NSPanel
@end
@implementation TCNotchPanel
- (BOOL)canBecomeKeyWindow { return NO; }
- (BOOL)canBecomeMainWindow { return NO; }
@end

@interface TCNotchButton : NSButton
@end
@implementation TCNotchButton
- (BOOL)acceptsFirstMouse:(NSEvent *)event { return YES; }
- (BOOL)acceptsFirstResponder { return NO; }
- (void)performClick:(id)sender {
    // NSButton's synthetic/accessibility click can activate the application
    // even inside a nonactivating panel. Dispatch its action without doing so.
    if (self.enabled) [NSApp sendAction:self.action to:self.target from:self];
}
@end

@interface TCWaveform : NSView
@property NSString *phase;
@property CGFloat level;
@property CGFloat motionTime;
@property BOOL reducedMotion;
@end

@interface TCNotchView : NSView
@property NSString *phase;
@property NSString *detail;
@property int64_t startedAt;
@property CGFloat safeTop;
@property CGFloat notchWidth;
@property CGFloat smoothedLevel;
@property CGFloat motionTime;
@property BOOL reducedMotion;
@property NSView *surface;
@property TCWaveform *waveform;
@property NSImageView *errorIcon;
@property NSTextField *status;
@property NSTextField *clock;
@property TCNotchButton *primary;
@property TCNotchButton *dismiss;
- (void)refresh;
- (void)tick;
- (NSTimeInterval)animateExpanded:(BOOL)expanded;
@end

static NSColor *accent(NSString *phase) {
    if ([phase isEqualToString:@"error"]) return [NSColor colorWithSRGBRed:1 green:.42 blue:.45 alpha:1];
    if ([phase isEqualToString:@"recording"] || [phase isEqualToString:@"done"])
        return [NSColor colorWithSRGBRed:.65 green:.91 blue:.73 alpha:1];
    return [NSColor colorWithSRGBRed:.70 green:.76 blue:1 alpha:1];
}
static NSTextField *label(CGFloat size, NSFontWeight weight) {
    NSTextField *field = [NSTextField labelWithString:@""];
    field.font = [NSFont systemFontOfSize:size weight:weight];
    field.textColor = NSColor.whiteColor;
    field.lineBreakMode = NSLineBreakByTruncatingTail;
    return field;
}
static CGPathRef surfacePathForWidth(NSRect bounds, CGFloat safeTop, CGFloat width) {
    // Solid black wings share the physical notch's bottom edge. The camera
    // occupies the untouched middle; all controls live in the two side wings.
    CGFloat left = NSMidX(bounds)-width/2, right = NSMidX(bounds)+width/2, bottom = gutter;
    CGFloat top = NSHeight(bounds), radius = 11, shoulder = 5;
    CGMutablePathRef path = CGPathCreateMutable();
    if (safeTop <= 0) {
        CGPathAddRoundedRect(path, NULL, CGRectMake(left,bottom,right-left,top-bottom-gutter), 16, 16);
        return path;
    }
    CGPathMoveToPoint(path, NULL, left-shoulder, top);
    CGPathAddLineToPoint(path, NULL, right+shoulder, top);
    CGPathAddQuadCurveToPoint(path, NULL, right,top, right,top-shoulder);
    CGPathAddLineToPoint(path, NULL, right, bottom+radius);
    CGPathAddQuadCurveToPoint(path, NULL, right,bottom, right-radius,bottom);
    CGPathAddLineToPoint(path, NULL, left+radius,bottom);
    CGPathAddQuadCurveToPoint(path, NULL, left,bottom, left,bottom+radius);
    CGPathAddLineToPoint(path, NULL, left,top-shoulder);
    CGPathAddQuadCurveToPoint(path, NULL, left,top, left-shoulder,top);
    CGPathCloseSubpath(path);
    return path;
}

static CGPathRef surfacePath(NSRect bounds, CGFloat safeTop, CGFloat notchWidth) {
    return surfacePathForWidth(bounds,safeTop,NSWidth(bounds)-2*gutter);
}

@implementation TCNotchView
- (instancetype)initWithFrame:(NSRect)frame {
    if (!(self = [super initWithFrame:frame])) return nil;
    self.wantsLayer = YES;
    _phase = @"idle";
    _surface = [[NSView alloc] initWithFrame:self.bounds];
    _surface.wantsLayer = YES;
    CAShapeLayer *surfaceMask = [CAShapeLayer layer];
    self.layer.mask = surfaceMask;
    [self addSubview:_surface];
    _waveform = [[TCWaveform alloc] initWithFrame:NSZeroRect];
    [self addSubview:_waveform];
    _errorIcon = [[NSImageView alloc] initWithFrame:NSZeroRect];
    _errorIcon.image = [[NSImage imageWithSystemSymbolName:@"exclamationmark.triangle.fill" accessibilityDescription:@"Error"]
        imageWithSymbolConfiguration:[NSImageSymbolConfiguration configurationWithPointSize:15 weight:NSFontWeightSemibold]];
    _errorIcon.contentTintColor = accent(@"error");
    _errorIcon.hidden = YES;
    [self addSubview:_errorIcon];
    _status = label(10, NSFontWeightMedium);
    _clock = label(10, NSFontWeightRegular);
    _clock.font = [NSFont monospacedDigitSystemFontOfSize:10 weight:NSFontWeightRegular];
    _clock.alignment = NSTextAlignmentRight;
    _clock.textColor = [NSColor colorWithWhite:1 alpha:.65];
    _primary = [[TCNotchButton alloc] initWithFrame:NSZeroRect];
    _primary.bordered = NO;
    _primary.bezelStyle = NSBezelStyleCircular;
    _primary.imagePosition = NSImageOnly;
    _primary.target = self;
    _primary.action = @selector(primaryAction:);
    _primary.wantsLayer = YES;
    _primary.layer.cornerRadius = 9;
    _dismiss = [[TCNotchButton alloc] initWithFrame:NSZeroRect];
    _dismiss.bordered = NO;
    _dismiss.image = [NSImage imageWithSystemSymbolName:@"xmark" accessibilityDescription:@"Cancel"];
    _dismiss.image = [_dismiss.image imageWithSymbolConfiguration:[NSImageSymbolConfiguration configurationWithPointSize:10 weight:NSFontWeightRegular]];
    _dismiss.contentTintColor = [NSColor colorWithWhite:1 alpha:.6];
    _dismiss.target = self;
    _dismiss.action = @selector(dismissAction:);
    for (NSView *view in @[_status, _clock, _primary, _dismiss]) [self addSubview:view];
    [self setAccessibilityElement:YES];
    [self setAccessibilityRole:NSAccessibilityGroupRole];
    return self;
}
- (BOOL)acceptsFirstMouse:(NSEvent *)event { return YES; }
- (void)primaryAction:(id)sender {
    if ([_phase isEqualToString:@"recording"]) { if (actionHandler) actionHandler(0); }
    else if ([_phase isEqualToString:@"error"] || [_phase isEqualToString:@"done"]) {
        if (actionHandler) actionHandler(2);
    }
}
- (void)dismissAction:(id)sender {
    if (![_phase isEqualToString:@"inserting"] && actionHandler) actionHandler(1);
}
- (void)layout {
    [super layout];
    CGFloat left = gutter+8, right = NSWidth(self.bounds)-gutter-8;
    CGFloat centerY = gutter + (_safeTop > 0 ? _safeTop : bodyHeight)/2;
    _surface.frame = self.bounds;
    CGPathRef path = surfacePath(self.bounds, _safeTop, _notchWidth);
    [CATransaction begin];
    [CATransaction setDisableActions:YES];
    ((CAShapeLayer *)self.layer.mask).path = path;
    [CATransaction commit];
    CGPathRelease(path);
    BOOL failed = [_phase isEqualToString:@"error"];
    _errorIcon.frame = NSMakeRect(left,centerY-9,18,18);
    _status.frame = NSMakeRect(left+(failed ? 23 : 0),centerY-7,failed ? 48 : 50,15);
    _waveform.frame = NSMakeRect(left+53,centerY-10,18,20);
    _clock.frame = NSMakeRect(right-72,centerY-7,36,15);
    _primary.frame = NSMakeRect(right-(failed ? 74 : 32),centerY-9,failed ? 60 : 18,18);
    _dismiss.frame = NSMakeRect(right-10,centerY-9,14,18);
}
- (NSTimeInterval)animateExpanded:(BOOL)expanded {
    [self layoutSubtreeIfNeeded];
    CAShapeLayer *mask = (CAShapeLayer *)self.layer.mask;
    CGFloat fullWidth = NSWidth(self.bounds)-2*gutter;
    CGFloat collapsedWidth = _safeTop > 0 ? _notchWidth : 100;
    CGPathRef from = surfacePathForWidth(self.bounds,_safeTop,expanded ? collapsedWidth : fullWidth);
    CGPathRef to = surfacePathForWidth(self.bounds,_safeTop,expanded ? fullWidth : collapsedWidth);
    // Reverse smoothly when a new recording interrupts the closing animation.
    CAShapeLayer *presentation = (CAShapeLayer *)mask.presentationLayer;
    if ([mask animationForKey:@"expansion"] && presentation.path) {
        CGPathRelease(from);
        from = CGPathCreateCopy(presentation.path);
    }
    NSTimeInterval duration = _reducedMotion ? 0 : expanded ? .38 : .25;
    [CATransaction begin];
    [CATransaction setDisableActions:YES];
    mask.path = to;
    [CATransaction commit];
    [mask removeAnimationForKey:@"expansion"];
    if (duration > 0) {
        CABasicAnimation *morph = [CABasicAnimation animationWithKeyPath:@"path"];
        morph.fromValue = (__bridge id)from;
        morph.toValue = (__bridge id)to;
        morph.duration = duration;
        morph.timingFunction = [CAMediaTimingFunction functionWithControlPoints:.2 : .85 : .2 :1];
        [mask addAnimation:morph forKey:@"expansion"];
    }
    for (NSView *control in @[_status,_waveform,_errorIcon,_clock,_primary,_dismiss]) {
        control.wantsLayer = YES;
        CGFloat target = expanded ? (control == _dismiss && !_dismiss.enabled ? .3 : 1) : 0;
        control.alphaValue = target;
        [control.layer removeAnimationForKey:@"reveal"];
        if (duration > 0) {
            CABasicAnimation *reveal = [CABasicAnimation animationWithKeyPath:@"opacity"];
            reveal.fromValue = expanded ? @0 : @(control.layer.presentationLayer.opacity);
            reveal.toValue = @(target);
            reveal.duration = expanded ? .18 : .1;
            reveal.beginTime = CACurrentMediaTime() + (expanded ? .12 : 0);
            reveal.fillMode = kCAFillModeBackwards;
            [control.layer addAnimation:reveal forKey:@"reveal"];
        }
    }
    CGPathRelease(from); CGPathRelease(to);
    return duration;
}
- (void)refresh {
    _reducedMotion = NSWorkspace.sharedWorkspace.accessibilityDisplayShouldReduceMotion;
    NSDictionary *titles = @{@"starting":@"Starting", @"recording":@"Listening",
        @"transcribing":@"Transcribing", @"inserting":@"Inserting",
        @"done":@"Saved", @"error":@"Error"};
    NSString *fullStatus = titles[_phase] ?: @"Transcribe";
    BOOL failed = [_phase isEqualToString:@"error"];
    _status.stringValue = [_phase isEqualToString:@"recording"] ? @"Live" : [_phase isEqualToString:@"transcribing"] ? @"Working" : fullStatus;
    _status.textColor = failed ? accent(_phase) : NSColor.whiteColor;
    _status.font = [NSFont systemFontOfSize:10 weight:failed ? NSFontWeightSemibold : NSFontWeightMedium];
    _status.toolTip = _detail.length ? _detail : fullStatus;
    _errorIcon.hidden = !failed;
    _errorIcon.toolTip = _status.toolTip;
    _waveform.hidden = failed;
    _clock.hidden = failed;
    BOOL recording = [_phase isEqualToString:@"recording"];
    BOOL terminal = [_phase isEqualToString:@"error"] || [_phase isEqualToString:@"done"];
    NSString *symbol = recording ? @"stop.fill" : failed ? @"clock.arrow.circlepath" :
        [_phase isEqualToString:@"done"] ? @"checkmark" : @"ellipsis";
    NSString *primaryLabel = recording ? @"Stop recording" : terminal ? @"History" : @"Processing";
    _primary.image = [[NSImage imageWithSystemSymbolName:symbol accessibilityDescription:primaryLabel]
        imageWithSymbolConfiguration:[NSImageSymbolConfiguration configurationWithPointSize:10 weight:NSFontWeightSemibold]];
    _primary.title = failed ? @"History" : @"";
    _primary.font = [NSFont systemFontOfSize:10 weight:NSFontWeightMedium];
    _primary.imagePosition = failed ? NSImageLeading : NSImageOnly;
    _primary.enabled = recording || terminal;
    _primary.toolTip = primaryLabel;
    [_primary setAccessibilityLabel:primaryLabel];
    _primary.contentTintColor = failed ? NSColor.whiteColor : accent(_phase);
    _primary.layer.backgroundColor = [(failed ? NSColor.whiteColor : accent(_phase)) colorWithAlphaComponent:.13].CGColor;
    _dismiss.enabled = ![_phase isEqualToString:@"inserting"];
    _dismiss.alphaValue = _dismiss.enabled ? 1 : .3;
    NSString *dismissLabel = terminal ? @"Dismiss" : @"Cancel recording";
    [_dismiss setAccessibilityLabel:dismissLabel];
    _dismiss.toolTip = _dismiss.enabled ? dismissLabel : @"Finishing insertion";
    _surface.layer.backgroundColor = NSColor.blackColor.CGColor;
    self.accessibilityLabel = [NSString stringWithFormat:@"Transcribe. %@%@", fullStatus,
        _detail.length ? [@". " stringByAppendingString:_detail] : @""];
    [self tick];
    self.needsLayout = YES;
}
- (void)tick {
    BOOL recording = [_phase isEqualToString:@"recording"];
    if (recording) {
        float sample = atomic_load(&inputLevel);
        CGFloat target = isfinite(sample) ? MIN(1, MAX(0, sample * 7)) : 0;
        _smoothedLevel += (target-_smoothedLevel) * (target > _smoothedLevel ? .5 : .16);
    } else _smoothedLevel = 0;
    if (!_reducedMotion) _motionTime += .033;
    // Freeze elapsed time after recording, and clear it for imports/startup.
    if (recording || !_startedAt) {
        NSInteger seconds = _startedAt ? MAX(0, (NSInteger)(NSDate.date.timeIntervalSince1970 - _startedAt/1000.)) : 0;
        _clock.stringValue = _startedAt ? [NSString stringWithFormat:@"%02ld:%02ld", (long)(seconds/60), (long)(seconds%60)] : @"";
    }
    _waveform.phase = _phase;
    _waveform.level = _smoothedLevel;
    _waveform.motionTime = _motionTime;
    _waveform.reducedMotion = _reducedMotion;
    _waveform.needsDisplay = YES;
}
@end

@implementation TCWaveform
- (void)drawRect:(NSRect)dirtyRect {
    [super drawRect:dirtyRect];
    CGFloat x = 0;
    [accent(_phase) setFill];
    BOOL recording = [_phase isEqualToString:@"recording"];
    BOOL busy = [@[@"starting", @"transcribing", @"inserting"] containsObject:_phase];
    for (NSInteger i=0; i<5; i++) {
        CGFloat envelope = .35 + .65 * sin((i+1)*M_PI/6);
        CGFloat level = recording ? _level * envelope : busy ? .18 + .28 * (1+sin(_motionTime*4-i*.5))/2 : .08;
        if (_reducedMotion && !recording) level = .15;
        CGFloat height = 2 + level*15;
        [[NSBezierPath bezierPathWithRoundedRect:NSMakeRect(x+i*3.5, 10-height/2, 2, height) xRadius:1.25 yRadius:1.25] fill];
    }

}
@end

@interface TCNotchController : NSObject
@property TCNotchPanel *panel;
@property TCNotchView *view;
@property NSTimer *timer;
@property BOOL requested;
@property NSUInteger generation;
@property NSUInteger ticks;
@property NSArray *observers;
- (void)position;
- (void)hide;
@end
static TCNotchController *controller;

// Prefer the focused application's front window. This is a read-only window
// list (no screenshot or Accessibility prompt). Mouse position is the fallback.
static NSScreen *destinationScreen(void) {
    NSArray<NSScreen *> *screens = NSScreen.screens;
    NSScreen *fallback = NSScreen.mainScreen ?: screens.firstObject;
    for (NSScreen *screen in screens) if (NSPointInRect(NSEvent.mouseLocation, screen.frame)) fallback = screen;
    pid_t pid = NSWorkspace.sharedWorkspace.frontmostApplication.processIdentifier;
    if (pid == NSProcessInfo.processInfo.processIdentifier) return fallback;
    NSArray *windows = CFBridgingRelease(CGWindowListCopyWindowInfo(kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements, kCGNullWindowID));
    CGFloat primaryTop = NSMaxY(screens.firstObject.frame);
    for (NSDictionary *window in windows) {
        if ([window[(id)kCGWindowOwnerPID] intValue] != pid || [window[(id)kCGWindowLayer] intValue] != 0) continue;
        CGRect bounds;
        if (!CGRectMakeWithDictionaryRepresentation((__bridge CFDictionaryRef)window[(id)kCGWindowBounds], &bounds)) continue;
        NSRect frame = NSMakeRect(bounds.origin.x, primaryTop-CGRectGetMaxY(bounds), bounds.size.width, bounds.size.height);
        CGFloat best = 0;
        for (NSScreen *screen in screens) {
            NSRect overlap = NSIntersectionRect(frame, screen.frame);
            CGFloat area = NSWidth(overlap)*NSHeight(overlap);
            if (area > best) { best = area; fallback = screen; }
        }
        break;
    }
    return fallback;
}

@implementation TCNotchController
- (instancetype)init {
    if (!(self = [super init])) return nil;
    _panel = [[TCNotchPanel alloc] initWithContentRect:NSMakeRect(0,0,392,114)
        styleMask:NSWindowStyleMaskBorderless | NSWindowStyleMaskNonactivatingPanel
        backing:NSBackingStoreBuffered defer:NO];
    _panel.releasedWhenClosed = NO;
    _panel.opaque = NO;
    _panel.backgroundColor = NSColor.clearColor;
    _panel.hasShadow = NO;
    _panel.hidesOnDeactivate = NO;
    _panel.floatingPanel = YES;
    _panel.becomesKeyOnlyIfNeeded = YES;
    _panel.level = NSStatusWindowLevel + 1;
    _panel.collectionBehavior = NSWindowCollectionBehaviorCanJoinAllSpaces |
        NSWindowCollectionBehaviorFullScreenAuxiliary | NSWindowCollectionBehaviorStationary |
        NSWindowCollectionBehaviorIgnoresCycle;
    _panel.title = @"Transcribe recording";
    _view = [[TCNotchView alloc] initWithFrame:NSMakeRect(0,0,392,114)];
    _panel.contentView = _view;
    __weak TCNotchController *weakSelf = self;
    NSMutableArray *observers = [NSMutableArray array];
    for (NSString *name in @[NSWorkspaceDidActivateApplicationNotification, NSWorkspaceActiveSpaceDidChangeNotification,
                             NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification]) {
        id token = [NSWorkspace.sharedWorkspace.notificationCenter addObserverForName:name object:nil queue:NSOperationQueue.mainQueue usingBlock:^(NSNotification *note) {
            TCNotchController *self = weakSelf;
            if (self.requested) { [self.view refresh]; [self position]; }
        }];
        [observers addObject:@[NSWorkspace.sharedWorkspace.notificationCenter, token]];
    }
    id token = [NSNotificationCenter.defaultCenter addObserverForName:NSApplicationDidChangeScreenParametersNotification object:nil queue:NSOperationQueue.mainQueue usingBlock:^(NSNotification *note) {
        TCNotchController *self = weakSelf;
        if (self.requested) [self position];
    }];
    [observers addObject:@[NSNotificationCenter.defaultCenter, token]];
    _observers = observers;
    return self;
}
- (void)position {
    if (!_requested) return;
    NSScreen *screen = destinationScreen();
    if (!screen) return;
    CGFloat safeTop = screen.safeAreaInsets.top;
    CGFloat notchWidth = 0;
    if (safeTop > 0) {
        notchWidth = NSMinX(screen.auxiliaryTopRightArea)-NSMaxX(screen.auxiliaryTopLeftArea);
        if (notchWidth <= 0) notchWidth = 180;
    }
    NSRect frame = panelFrame(screen.frame, screen.visibleFrame, safeTop, notchWidth);
    _view.safeTop = safeTop;
    _view.notchWidth = notchWidth;
    if (!NSEqualRects(_panel.frame, frame)) [_panel setFrame:frame display:YES];
    _view.needsLayout = YES;
    [_panel orderFrontRegardless];
}
- (void)hide {
    _requested = NO;
    NSUInteger generation = ++_generation;
    [_timer invalidate]; _timer = nil;
    atomic_store(&inputLevel, 0);
    NSTimeInterval duration = [_view animateExpanded:NO];
    if (duration == 0) { [_panel orderOut:nil]; return; }
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, (int64_t)(duration*NSEC_PER_SEC)), dispatch_get_main_queue(), ^{
        // A new recording can start before contraction has finished.
        if (!self.requested && generation == self.generation) [self.panel orderOut:nil];
    });
}
- (void)dealloc {
    [_timer invalidate];
    for (NSArray *pair in _observers) [(NSNotificationCenter *)pair[0] removeObserver:pair[1]];
    [_panel orderOut:nil];
}
@end

void tc_notch_init(TCNotchAction action) {
    NSCAssert(NSThread.isMainThread, @"Notch UI requires the main thread");
    actionHandler = action;
    if (!controller) controller = [TCNotchController new];
}
void tc_notch_update(const char *phase, int64_t started_at, const char *error) {
    NSCAssert(NSThread.isMainThread, @"Notch UI requires the main thread");
    if (!controller) return;
    NSString *next = [NSString stringWithUTF8String:phase] ?: @"idle";
    if ([next isEqualToString:@"idle"]) { tc_notch_hide(); return; }
    BOOL entering = !controller.requested;
    BOOL changed = ![controller.view.phase isEqualToString:next];
    controller.requested = YES;
    controller.generation++;
    if (controller.view.startedAt != started_at) controller.view.clock.stringValue = @"";
    controller.view.phase = next;
    controller.view.startedAt = started_at;
    controller.view.detail = error ? [NSString stringWithUTF8String:error] : @"";
    [controller.view refresh];
    [controller position];
    if (entering) {
        controller.panel.alphaValue = 1;
        [controller.view animateExpanded:YES];
        __weak TCNotchController *weakController = controller;
        controller.timer = [NSTimer timerWithTimeInterval:1./30 repeats:YES block:^(NSTimer *timer) {
            TCNotchController *current = weakController;
            if (!current.requested) return;
            [current.view tick];
            // Movement, menu-bar changes, and native visibility recovery.
            if (++current.ticks % 15 == 0) [current position];
        }];
        [NSRunLoop.mainRunLoop addTimer:controller.timer forMode:NSRunLoopCommonModes];
    }
    if (changed) NSAccessibilityPostNotificationWithUserInfo(controller.view, NSAccessibilityAnnouncementRequestedNotification,
        @{NSAccessibilityAnnouncementKey:controller.view.accessibilityLabel, NSAccessibilityPriorityKey:@(NSAccessibilityPriorityMedium)});
}
void tc_notch_level(float level) { atomic_store(&inputLevel, level); }
void tc_notch_hide(void) {
    NSCAssert(NSThread.isMainThread, @"Notch UI requires the main thread");
    if (controller.requested) [controller hide];
}
void tc_notch_destroy(void) {
    NSCAssert(NSThread.isMainThread, @"Notch UI requires the main thread");
    [controller hide];
    controller = nil;
    actionHandler = NULL;
}
