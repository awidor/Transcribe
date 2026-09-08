#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#include <unistd.h>
#include <stdbool.h>

@interface TCTarget : NSObject
@property pid_t pid;
@property AXUIElementRef element;
@property BOOL terminal;
@end
@implementation TCTarget
- (void)dealloc { if (_element) CFRelease(_element); }
@end

static AXUIElementRef focused(void) {
    AXUIElementRef system = AXUIElementCreateSystemWide();
    CFTypeRef item = NULL;
    AXError error = AXUIElementCopyAttributeValue(system,kAXFocusedUIElementAttribute,&item);
    CFRelease(system);
    if (error != kAXErrorSuccess || !item || CFGetTypeID(item) != AXUIElementGetTypeID()) {
        if (item) CFRelease(item);
        return NULL;
    }
    return (AXUIElementRef)item;
}
static BOOL matches(TCTarget *target) {
    if (NSWorkspace.sharedWorkspace.frontmostApplication.processIdentifier != target.pid) return NO;
    AXUIElementRef item = focused();
    BOOL same = item && CFEqual(item,target.element);
    if (item) CFRelease(item); return same;
}
// Status: 0 = captured, 1 = permission denied, 2 = no external application,
// 3 = focused element unavailable, 4 = destination changed during capture.
// A null target alone does not establish that Accessibility is denied.
void *tc_capture(int *status) {
    @autoreleasepool {
        // A paste failure is reported in History; never reopen a permission
        // prompt for every recording when macOS rejects the app's identity.
        *status = 1;
        if (!AXIsProcessTrusted()) return NULL;
        *status = 2;
        NSRunningApplication *app = NSWorkspace.sharedWorkspace.frontmostApplication;
        if (!app || app.processIdentifier == getpid()) return NULL;
        *status = 3;
        AXUIElementRef item = focused(); if (!item) return NULL;
        pid_t elementPid = 0;
        if (AXUIElementGetPid(item,&elementPid) != kAXErrorSuccess) {
            CFRelease(item); return NULL;
        }
        if (elementPid != app.processIdentifier) {
            *status = 4; CFRelease(item); return NULL;
        }
        TCTarget *target = [TCTarget new]; target.pid = app.processIdentifier; target.element = item;
        NSString *bundle = app.bundleIdentifier.lowercaseString ?: @"";
        for (NSString *name in @[@"terminal",@"iterm",@"wezterm",@"ghostty",@"alacritty",@"kitty",@"warp"]) {
            if ([bundle containsString:name]) target.terminal = YES;
        }
        CFTypeRef description = NULL;
        AXUIElementCopyAttributeValue(item,kAXDescriptionAttribute,&description);
        if (description && CFGetTypeID(description)==CFStringGetTypeID()) {
            if ([(__bridge NSString *)description localizedCaseInsensitiveContainsString:@"terminal"]) target.terminal = YES;
        }
        if (description) CFRelease(description);
        *status = 0;
        return (__bridge_retained void *)target;
    }
}
void tc_release(void *p) { if (p) CFRelease(p); }
bool tc_terminal(void *p) { return ((__bridge TCTarget *)p).terminal; }
static BOOL modifiersReleased(void) {
    CGEventFlags flags = CGEventSourceFlagsState(kCGEventSourceStateCombinedSessionState);
    return !(flags & (kCGEventFlagMaskCommand|kCGEventFlagMaskShift|kCGEventFlagMaskAlternate|kCGEventFlagMaskControl));
}
int tc_paste(void *p, const char *utf8) {
    @autoreleasepool {
        TCTarget *target = (__bridge TCTarget *)p;
        if (!AXIsProcessTrusted()) return 2;
        for (int i=0; i<100 && !modifiersReleased(); i++) usleep(20000);
        if (!modifiersReleased()) return 3;
        if (!matches(target)) return 1;
        NSPasteboard *board = NSPasteboard.generalPasteboard;
        NSInteger before = board.changeCount;
        NSMutableArray<NSPasteboardItem *> *saved = [NSMutableArray new];
        for (NSPasteboardItem *original in board.pasteboardItems) {
            NSPasteboardItem *item = [NSPasteboardItem new];
            for (NSPasteboardType type in original.types) {
                NSData *data = [original dataForType:type];
                if (!data) return 4;
                [item setData:data forType:type];
            }
            [saved addObject:item];
        }
        if (board.changeCount != before || !matches(target)) return 1;
        NSString *text = [NSString stringWithUTF8String:utf8];
        [board clearContents]; [board setString:text forType:NSPasteboardTypeString];
        [board setData:[NSData data] forType:@"org.nspasteboard.TransientType"];
        NSInteger ours = board.changeCount;
        int result = 0;
        if (!matches(target) || !modifiersReleased()) result = 1;
        else {
            CGEventRef down = CGEventCreateKeyboardEvent(NULL,9,true);
            CGEventRef up = CGEventCreateKeyboardEvent(NULL,9,false);
            if (!down || !up) result = 4;
            else { CGEventSetFlags(down,kCGEventFlagMaskCommand); CGEventSetFlags(up,kCGEventFlagMaskCommand);
                CGEventPost(kCGHIDEventTap,down); CGEventPost(kCGHIDEventTap,up); usleep(1200000); }
            if (down) CFRelease(down); if (up) CFRelease(up);
        }
        // Never overwrite a newer clipboard owned by another user action.
        if (board.changeCount == ours) { [board clearContents]; if (saved.count) [board writeObjects:saved]; }
        return result;
    }
}
bool tc_copy(const char *utf8) {
    @autoreleasepool { NSPasteboard *board = NSPasteboard.generalPasteboard;
        [board clearContents]; return [board setString:[NSString stringWithUTF8String:utf8] forType:NSPasteboardTypeString]; }
}
