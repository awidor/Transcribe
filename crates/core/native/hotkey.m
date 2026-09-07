#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#import <Carbon/Carbon.h>
#import <IOKit/hidsystem/IOLLEvent.h>
#include <pthread.h>
#include <stdbool.h>
#include <stdint.h>
#include <unistd.h>

typedef bool (*TCKeyCallback)(void *,uint32_t,const char *,bool,bool);
typedef struct { void *context; TCKeyCallback callback; CFMachPortRef tap; } TCKeyContext;

// macOS 27 asserts on input-source lookups from the event-tap thread. Resolve
// both unshifted and shifted labels on the main queue, then publish a snapshot.
// The tap must never synchronously wait for the UI (including during startup).
static pthread_mutex_t labelLock=PTHREAD_MUTEX_INITIALIZER;
static NSDictionary<NSNumber *,NSString *> *layoutLabels;
static void refreshKeyLabels(void) {
    dispatch_assert_queue(dispatch_get_main_queue());
    NSMutableDictionary *labels=[NSMutableDictionary dictionary];
    TISInputSourceRef source=TISCopyCurrentKeyboardLayoutInputSource();
    CFDataRef data=source ? TISGetInputSourceProperty(source,kTISPropertyUnicodeKeyLayoutData) : NULL;
    if (data) {
        const UCKeyboardLayout *layout=(const UCKeyboardLayout *)CFDataGetBytePtr(data);
        UInt32 keyboardType=LMGetKbdType();
        for (unsigned shifted=0; shifted<2; shifted++) {
            for (CGKeyCode key=0; key<128; key++) {
                UniChar characters[255];
                UniCharCount length=0;
                UInt32 deadKeyState=0;
                OSStatus status=UCKeyTranslate(layout,key,kUCKeyActionDisplay,
                    shifted ? (shiftKey >> 8) : 0,keyboardType,kUCKeyTranslateNoDeadKeysMask,
                    &deadKeyState,255,&length,characters);
                if (status==noErr && length && characters[0]>=32) {
                    labels[@(key+128*shifted)]=[[NSString stringWithCharacters:characters length:length] uppercaseString];
                }
            }
        }
    }
    if (source) CFRelease(source);
    NSDictionary *snapshot=[labels copy];
    pthread_mutex_lock(&labelLock);
    layoutLabels=snapshot;
    pthread_mutex_unlock(&labelLock);
}
static void prepareKeyLabels(void) {
    static dispatch_once_t once;
    dispatch_once(&once,^{
        dispatch_async(dispatch_get_main_queue(),^{
            [[NSDistributedNotificationCenter defaultCenter]
                addObserverForName:(__bridge NSString *)kTISNotifySelectedKeyboardInputSourceChanged
                object:nil queue:[NSOperationQueue mainQueue]
                usingBlock:^(NSNotification *notification) {
                    (void)notification;
                    refreshKeyLabels();
                }];
            refreshKeyLabels();
        });
    });
}
static NSString *keyLabel(CGKeyCode key, CGEventRef event) {
    NSDictionary *names=@{@54:@"Right Command",@55:@"Left Command",@56:@"Left Shift",@60:@"Right Shift",
        @58:@"Left Alt",@61:@"Right Alt",@59:@"Left Ctrl",@62:@"Right Ctrl",@63:@"Fn",@57:@"Caps Lock",
        @36:@"Enter",@48:@"Tab",@49:@"Space",@51:@"Backspace",@53:@"Escape",@117:@"Delete",
        @123:@"Left",@124:@"Right",@125:@"Down",@126:@"Up",@115:@"Home",@119:@"End",@116:@"Page Up",@121:@"Page Down",
        @122:@"F1",@120:@"F2",@99:@"F3",@118:@"F4",@96:@"F5",@97:@"F6",@98:@"F7",@100:@"F8",@101:@"F9",@109:@"F10",
        @103:@"F11",@111:@"F12",@105:@"F13",@107:@"F14",@113:@"F15",@106:@"F16",@64:@"F17",@79:@"F18",@80:@"F19",@90:@"F20",
        @76:@"Numpad Enter",@71:@"Clear"};
    if (names[@(key)]) return names[@(key)];
    unsigned shifted=(CGEventGetFlags(event)&kCGEventFlagMaskShift) ? 1 : 0;
    pthread_mutex_lock(&labelLock);
    NSString *label=key<128 ? layoutLabels[@(key+128*shifted)] : nil;
    pthread_mutex_unlock(&labelLock);
    if (label) return label;
    return [NSString stringWithFormat:@"Key %u",key];
}
static CGEventRef keyEvent(CGEventTapProxy proxy, CGEventType type, CGEventRef event, void *opaque) {
    (void)proxy;
    TCKeyContext *ctx=opaque;
    if (type==kCGEventTapDisabledByTimeout || type==kCGEventTapDisabledByUserInput) {
        ctx->callback(ctx->context,UINT32_MAX,"",false,false);
        CGEventTapEnable(ctx->tap,true); return event;
    }
    if (CGEventGetIntegerValueField(event,kCGEventSourceUnixProcessID)==getpid()) return event;
    @autoreleasepool {
        if (type==NX_SYSDEFINED) {
            NSEvent *native=[NSEvent eventWithCGEvent:event];
            if (native.subtype!=8) return event;
            uint32_t code=(uint32_t)((native.data1>>16)&0xffff);
            bool down=((native.data1>>8)&0xff)==0xA;
            NSDictionary *names=@{@0:@"Volume Up",@1:@"Volume Down",@2:@"Brightness Up",@3:@"Brightness Down",
                @7:@"Mute",@14:@"Keyboard Brightness Up",@15:@"Keyboard Brightness Down",@16:@"Play / Pause",@17:@"Next Track",@18:@"Previous Track"};
            NSString *name=names[@(code)] ?: [NSString stringWithFormat:@"Media %u",code];
            if (ctx->callback(ctx->context,256+code,name.UTF8String,down,false)) return NULL;
            return event;
        }
        CGKeyCode key=(CGKeyCode)CGEventGetIntegerValueField(event,kCGKeyboardEventKeycode);
        bool down=type==kCGEventKeyDown;
        bool modifier=type==kCGEventFlagsChanged && key!=57;
        NSString *name=keyLabel(key,event);
        if (type==kCGEventFlagsChanged) {
            CGEventFlags flags=CGEventGetFlags(event);
            if (key==57) {
                bool suppress=ctx->callback(ctx->context,key,name.UTF8String,true,false);
                suppress=ctx->callback(ctx->context,key,name.UTF8String,false,false)||suppress;
                return suppress ? NULL : event;
            }
            // Device-specific bits distinguish left/right modifiers even with both held.
            uint64_t mask=key==59?0x1:key==56?0x2:key==60?0x4:key==55?0x8:key==54?0x10:
                key==58?0x20:key==61?0x40:key==62?0x2000:key==63?kCGEventFlagMaskSecondaryFn:0;
            down=(flags&mask)!=0;
        }
        return ctx->callback(ctx->context,key,name.UTF8String,down,modifier) ? NULL : event;
    }
}
void tc_hotkey_start(void *context, TCKeyCallback callback, void (*ready)(void *,int)) {
    @autoreleasepool {
        // Checking on startup or Retry must not repeatedly open system prompts.
        // A stale signing requirement can deny access even with the switch on.
        if (!AXIsProcessTrusted()) { ready(context,1); return; }
        prepareKeyLabels();
        TCKeyContext ctx={context,callback,NULL};
        CGEventMask mask=CGEventMaskBit(kCGEventKeyDown)|CGEventMaskBit(kCGEventKeyUp)|CGEventMaskBit(kCGEventFlagsChanged)|CGEventMaskBit(NX_SYSDEFINED);
        ctx.tap=CGEventTapCreate(kCGSessionEventTap,kCGHeadInsertEventTap,kCGEventTapOptionDefault,mask,keyEvent,&ctx);
        if (!ctx.tap) { ready(context,CGPreflightListenEventAccess() ? 3 : 2); return; }
        CFRunLoopSourceRef source=CFMachPortCreateRunLoopSource(kCFAllocatorDefault,ctx.tap,0);
        if (!source) { CFRelease(ctx.tap); ready(context,3); return; }
        CFRunLoopAddSource(CFRunLoopGetCurrent(),source,kCFRunLoopCommonModes);
        CGEventTapEnable(ctx.tap,true);
        if (!CGEventTapIsEnabled(ctx.tap)) {
            CFRunLoopRemoveSource(CFRunLoopGetCurrent(),source,kCFRunLoopCommonModes);
            CFMachPortInvalidate(ctx.tap); CFRelease(source); CFRelease(ctx.tap);
            ready(context,3); return;
        }
        ready(context,0); CFRunLoopRun();
        CFRunLoopRemoveSource(CFRunLoopGetCurrent(),source,kCFRunLoopCommonModes);
        CFMachPortInvalidate(ctx.tap); CFRelease(source); CFRelease(ctx.tap);
    }
}
