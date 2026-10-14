// Local visual acceptance fixture. No microphone, credentials, provider calls,
// clipboard access, or insertion. The panel is the production implementation.
#import "notch.m"
static int64_t startedAt;
static void previewAction(int value) {
    if (value != 0) { tc_notch_hide(); return; }
    tc_notch_update("transcribing",startedAt,NULL,NULL,NULL);
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 3*NSEC_PER_SEC), dispatch_get_main_queue(), ^{
        if (controller.requested) tc_notch_update("cleaning",startedAt,NULL,NULL,NULL);
    });
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 5*NSEC_PER_SEC), dispatch_get_main_queue(), ^{
        if (!controller.requested) return;
        tc_notch_update("inserting",startedAt,NULL,NULL,NULL);
        dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 3*NSEC_PER_SEC), dispatch_get_main_queue(), ^{
            if (!controller.requested) return;
            tc_notch_update("done",startedAt,NULL,NULL,NULL);
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
        tc_notch_update("recording",startedAt,NULL,NULL,NULL);
        // Words arrive as they would while speaking.
        NSArray *spoken = [@"Hello, this is a preview of the words that appear while you speak, newest at the end." componentsSeparatedByString:@" "];
        __block NSUInteger count = 0;
        [NSTimer scheduledTimerWithTimeInterval:.3 repeats:YES block:^(NSTimer *timer) {
            if (![controller.view.phase isEqualToString:@"recording"]) { [timer invalidate]; return; }
            count = MIN(count+1, spoken.count);
            NSString *text = [[spoken subarrayWithRange:NSMakeRange(0,count-1)] componentsJoinedByString:@" "];
            NSString *interim = [(count > 1 ? @" " : @"") stringByAppendingString:spoken[count-1]];
            tc_notch_update("recording",startedAt,NULL,text.UTF8String,interim.UTF8String);
        }];
        [NSTimer scheduledTimerWithTimeInterval:.05 repeats:YES block:^(NSTimer *timer) {
            tc_notch_level(.04 + .06 * (1+sin(NSDate.date.timeIntervalSince1970*3))/2);
        }];
        [NSApp run];
    }
}
