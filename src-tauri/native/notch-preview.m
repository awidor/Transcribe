// Local visual acceptance fixture. No microphone, credentials, provider calls,
// clipboard access, or insertion. The panel is the production implementation.
#import "notch.m"
static int64_t startedAt;
static void previewAction(int value) {
    if (value != 0) { tc_notch_hide(); return; }
    tc_notch_update("transcribing",startedAt,NULL);
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 3*NSEC_PER_SEC), dispatch_get_main_queue(), ^{
        if (controller.requested) tc_notch_update("cleaning",startedAt,NULL);
    });
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 5*NSEC_PER_SEC), dispatch_get_main_queue(), ^{
        if (!controller.requested) return;
        tc_notch_update("inserting",startedAt,NULL);
        dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 3*NSEC_PER_SEC), dispatch_get_main_queue(), ^{
            if (!controller.requested) return;
            tc_notch_update("done",startedAt,NULL);
            dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 3*NSEC_PER_SEC), dispatch_get_main_queue(), ^{ tc_notch_hide(); });
        });
    });
}
int main(void) {
    @autoreleasepool {
        [NSApplication sharedApplication];
        [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
        tc_notch_init(previewAction);
        startedAt = (int64_t)(NSDate.date.timeIntervalSince1970*1000);
        tc_notch_update("recording",startedAt,NULL);
        [NSTimer scheduledTimerWithTimeInterval:.05 repeats:YES block:^(NSTimer *timer) {
            tc_notch_level(.04 + .06 * (1+sin(NSDate.date.timeIntervalSince1970*3))/2);
        }];
        [NSApp run];
    }
}
