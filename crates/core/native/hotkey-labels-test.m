// Exercise the real callback on a worker without installing a tap or posting input.
#import "hotkey.m"
#include <assert.h>
#include <stdio.h>
#include <string.h>

typedef struct {
    uint32_t code;
    const char *label;
    bool down;
    bool modifier;
    bool suppress;
    unsigned calls;
} Expected;
static bool receive(void *opaque,uint32_t code,const char *label,bool down,bool modifier) {
    Expected *expected=opaque;
    assert(![NSThread isMainThread]);
    assert(code==expected->code);
    assert(strcmp(label,expected->label)==0);
    assert(down==expected->down && modifier==expected->modifier);
    expected->calls++;
    return expected->suppress;
}
static void checkKey(CGKeyCode key,CGEventType type,CGEventFlags flags,NSString *label,bool suppress) {
    CGEventRef event=CGEventCreateKeyboardEvent(NULL,key,type==kCGEventKeyDown);
    assert(event);
    CGEventSetType(event,type);
    CGEventSetFlags(event,flags);
    CGEventSetIntegerValueField(event,kCGEventSourceUnixProcessID,0);
    Expected expected={key,label.UTF8String,type==kCGEventKeyDown,false,suppress,0};
    if (type==kCGEventFlagsChanged) {
        expected.down=(flags!=0);
        expected.modifier=true;
    }
    TCKeyContext context={&expected,receive,NULL};
    assert(keyEvent(NULL,type,event,&context)==(suppress ? NULL : event));
    assert(expected.calls==1);
    CFRelease(event);
}
int main(void) {
    @autoreleasepool {
        // Initialize through the same asynchronous main-queue path as startup.
        prepareKeyLabels();
        NSDate *deadline=[NSDate dateWithTimeIntervalSinceNow:5];
        while (!layoutLabels && [deadline timeIntervalSinceNow]>0) {
            [[NSRunLoop mainRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.01]];
        }
        assert(layoutLabels.count>0);
        NSDictionary *initial=layoutLabels;
        // Compare printable labels with the previous AppKit behavior, safely
        // evaluated on the main thread, including shifted punctuation.
        for (NSNumber *code in @[@0,@1,@6,@16,@18,@24,@27,@33,@39,@50,@65,@82]) {
            for (unsigned shifted=0; shifted<2; shifted++) {
                CGEventRef event=CGEventCreateKeyboardEvent(NULL,code.unsignedShortValue,true);
                CGEventSetFlags(event,shifted ? kCGEventFlagMaskShift : 0);
                NSString *expected=[NSEvent eventWithCGEvent:event].charactersIgnoringModifiers.uppercaseString;
                assert([keyLabel(code.unsignedShortValue,event) isEqualToString:expected]);
                CFRelease(event);
            }
        }
        dispatch_group_t work=dispatch_group_create();
        dispatch_group_async(work,dispatch_get_global_queue(QOS_CLASS_USER_INITIATED,0),^{
            @autoreleasepool {
                // Check every key, including unknown flags-changed keys, on the
                // thread that previously crashed in charactersIgnoringModifiers.
                for (CGKeyCode key=0; key<128; key++) {
                    CGEventRef event=CGEventCreateKeyboardEvent(NULL,key,true);
                    assert(event);
                    for (unsigned shifted=0; shifted<2; shifted++) {
                        CGEventSetFlags(event,shifted ? kCGEventFlagMaskShift : 0);
                        NSString *label=keyLabel(key,event);
                        assert(label.length>0);
                        checkKey(key,kCGEventKeyDown,CGEventGetFlags(event),label,true);
                        checkKey(key,kCGEventKeyUp,CGEventGetFlags(event),label,false);
                    }
                    CFRelease(event);
                }
                // Ctrl/Alt/Command must not change the printable key's label.
                NSString *letter=initial[@0];
                assert(letter.length>0);
                checkKey(0,kCGEventKeyDown,kCGEventFlagMaskControl|kCGEventFlagMaskAlternate|kCGEventFlagMaskCommand,letter,false);
                checkKey(49,kCGEventKeyDown,0,@"Space",true);
                checkKey(60,kCGEventFlagsChanged,0x4,@"Right Shift",true);
                checkKey(60,kCGEventFlagsChanged,0,@"Right Shift",true);
                checkKey(127,kCGEventFlagsChanged,0,@"Key 127",false);
            }
        });
        // Deliberately block the main queue: callbacks must complete without it.
        assert(dispatch_group_wait(work,dispatch_time(DISPATCH_TIME_NOW,5*NSEC_PER_SEC))==0);

        // A replacement snapshot is visible on the worker; no system layout change.
        pthread_mutex_lock(&labelLock);
        layoutLabels=@{@0:@"Ä",@128:@"!"};
        pthread_mutex_unlock(&labelLock);
        dispatch_group_async(work,dispatch_get_global_queue(QOS_CLASS_USER_INITIATED,0),^{
            @autoreleasepool {
                checkKey(0,kCGEventKeyDown,0,@"Ä",true);
                checkKey(0,kCGEventKeyDown,kCGEventFlagMaskShift,@"!",true);
                checkKey(1,kCGEventKeyDown,0,@"Key 1",false);
            }
        });
        assert(dispatch_group_wait(work,dispatch_time(DISPATCH_TIME_NOW,5*NSEC_PER_SEC))==0);
        refreshKeyLabels();
        assert([layoutLabels isEqualToDictionary:initial]);
        puts("PASS: background key callbacks, suppression, modifiers, Unicode/layout refresh, fallback, and nonblocking main-queue behavior");
    }
}
