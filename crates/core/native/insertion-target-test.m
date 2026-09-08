// Deterministic capture fixture. No permission prompts, focus changes, input,
// clipboard writes, or real cross-process Accessibility queries.
#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#include <assert.h>
#include <stdio.h>
#include <unistd.h>

static bool trusted;
static AXError focusError=kAXErrorNoValue;
static CFTypeRef focusValue;
static pid_t elementPid;
static int focusQueries;

@interface TCFixtureApplication : NSObject
@property pid_t processIdentifier;
@property NSString *bundleIdentifier;
@end
@implementation TCFixtureApplication
@end
static TCFixtureApplication *frontmost;
@interface TCFixtureWorkspace : NSObject
+ (instancetype)sharedWorkspace;
- (NSRunningApplication *)frontmostApplication;
@end
@implementation TCFixtureWorkspace
+ (instancetype)sharedWorkspace {
    static TCFixtureWorkspace *workspace;
    if (!workspace) workspace=[TCFixtureWorkspace new];
    return workspace;
}
- (NSRunningApplication *)frontmostApplication { return (NSRunningApplication *)frontmost; }
@end
static Boolean fixtureTrusted(void) { return trusted; }
static AXError fixtureAttribute(AXUIElementRef element,CFStringRef attribute,CFTypeRef *value) {
    *value=NULL;
    if (!CFEqual(attribute,kAXFocusedUIElementAttribute)) return kAXErrorNoValue;
    focusQueries++;
    if (focusValue) *value=CFRetain(focusValue);
    return focusError;
}
static AXError fixturePid(AXUIElementRef element,pid_t *pid) {
    *pid=elementPid;
    return kAXErrorSuccess;
}
#define NSWorkspace TCFixtureWorkspace
#define AXIsProcessTrusted fixtureTrusted
#define AXUIElementCopyAttributeValue fixtureAttribute
#define AXUIElementGetPid fixturePid
#import "macos.m"

int main(void) {
    @autoreleasepool {
        int status=-1;
        assert(tc_capture(&status)==NULL && status==1 && focusQueries==0);
        trusted=true;
        assert(tc_capture(&status)==NULL && status==2 && focusQueries==0);
        frontmost=[TCFixtureApplication new];
        frontmost.processIdentifier=getpid();
        frontmost.bundleIdentifier=@"app.transcribe.desktop";
        assert(tc_capture(&status)==NULL && status==2 && focusQueries==0);

        frontmost.processIdentifier=getpid()+1;
        frontmost.bundleIdentifier=@"fixture.editor";
        for (NSNumber *error in @[@(kAXErrorNoValue),@(kAXErrorCannotComplete),@(kAXErrorAttributeUnsupported)]) {
            focusError=error.intValue;
            assert(tc_capture(&status)==NULL && status==3);
        }
        focusError=kAXErrorSuccess;
        focusValue=CFSTR("not an Accessibility element");
        assert(tc_capture(&status)==NULL && status==3);

        AXUIElementRef element=AXUIElementCreateApplication(getpid());
        focusValue=element;
        elementPid=getpid()+2;
        assert(tc_capture(&status)==NULL && status==4);
        elementPid=frontmost.processIdentifier;
        void *target=tc_capture(&status);
        assert(target && status==0 && !tc_terminal(target));
        tc_release(target);
        frontmost.bundleIdentifier=@"com.apple.Terminal";
        target=tc_capture(&status);
        assert(target && status==0 && tc_terminal(target));
        tc_release(target);
        CFRelease(element);
        puts("PASS: denied permission, own/no app, unavailable/invalid focus, changed destination, and successful capture remain distinct");
    }
}
