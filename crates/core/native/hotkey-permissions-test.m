// Native startup policy fixture: no OS prompts, keyboard tap, or input capture.
#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#include <assert.h>
#include <stdio.h>
static BOOL trusted;
static bool listening;
static int prompts;
static int taps;
static Boolean testTrusted(void) { return trusted; }
static Boolean testPrompt(CFDictionaryRef options) { prompts++; return trusted; }
static bool testListening(void) { return listening; }
static CFMachPortRef testTap(CGEventTapLocation tap, CGEventTapPlacement place,
    CGEventTapOptions options, CGEventMask mask, CGEventTapCallBack callback, void *context) {
    taps++;
    return NULL;
}
#define AXIsProcessTrusted testTrusted
#define AXIsProcessTrustedWithOptions testPrompt
#define CGPreflightListenEventAccess testListening
#define CGEventTapCreate testTap
#import "hotkey.m"
static void ready(void *context, int status) { *(int *)context = status; }
static bool event(void *context, uint32_t code, const char *label, bool down, bool modifier) { return false; }
int main(void) {
    int status = -1;
    (void)testPrompt; // Remains available to detect accidental prompting.
    for (int retry=0; retry<3; retry++) {
        tc_hotkey_start(&status,event,ready);
        assert(status==1 && taps==0 && prompts==0);
    }
    trusted=YES;
    tc_hotkey_start(&status,event,ready);
    assert(status==2 && taps==1 && prompts==0);
    listening=true;
    tc_hotkey_start(&status,event,ready);
    assert(status==3 && taps==2 && prompts==0);
    puts("PASS: retries never prompt; Accessibility, Input Monitoring, and tap failure remain distinct");
}
