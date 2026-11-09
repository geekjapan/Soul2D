#import <Cocoa/Cocoa.h>
#import <InputMethodKit/InputMethodKit.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static NSString *controlDirectory;
static NSString *expectedClientBundle;
static NSUInteger traceSequence;
static NSUInteger requestSequence;
static NSUInteger lastControlSequence;
static NSDictionary *lastControl;
static NSUInteger armedRequest;
static NSUInteger inputDepth;
static int serverStatus;

static NSString *pointer(id object) {return [NSString stringWithFormat:@"%p", (__bridge void *)object];}

static void require(BOOL condition, NSString *reason) {
    if (!condition) @throw [NSException exceptionWithName:@"FixtureError" reason:reason userInfo:nil];
}

static void emit(NSDictionary *fields) {
    if (++traceSequence > 4096) {fputs("server trace limit\n", stderr); _Exit(2);}
    NSMutableDictionary *row = [fields mutableCopy];
    row[@"seq"] = @(traceSequence); row[@"pid"] = @(getpid());
    row[@"input_depth"] = @(inputDepth);
    NSData *bytes = [NSJSONSerialization dataWithJSONObject:row options:NSJSONWritingSortedKeys error:nil];
    if (!bytes || fwrite(bytes.bytes, 1, bytes.length, stdout) != bytes.length || fputc('\n', stdout) == EOF) _Exit(2);
    fflush(stdout);
}

static NSUInteger number(id value, NSUInteger maximum) {
    require([value isKindOfClass:NSNumber.class] && CFGetTypeID((__bridge CFTypeRef)value) != CFBooleanGetTypeID(), @"integer required");
    double scalar = [value doubleValue];
    require(isfinite(scalar) && scalar >= 1 && scalar <= maximum && floor(scalar) == scalar, @"integer outside fixture bounds");
    return (NSUInteger)scalar;
}

static NSDictionary *acceptControl(id value) {
    require([value isKindOfClass:NSDictionary.class], @"control dictionary required");
    NSDictionary *control = value;
    NSUInteger sequence = number(control[@"seq"], 4096);
    if (sequence == lastControlSequence) {
        require([control isEqual:lastControl], @"control sequence reused with changed payload");
        return nil;
    }
    require(sequence == lastControlSequence + 1, @"control sequence gap or reversal");
    NSString *operation = control[@"operation"];
    require([operation isKindOfClass:NSString.class], @"control operation required");
    BOOL stop = [operation isEqual:@"stop"];
    require(stop || [operation isEqual:@"release"] || [operation isEqual:@"arm"], @"unknown control operation");
    NSSet *keys = [NSSet setWithArray:control.allKeys];
    require([keys isEqual:[NSSet setWithArray:stop ? @[@"seq", @"operation"] : @[@"seq", @"operation", @"request"]]], @"unexpected control fields");
    if (!stop) number(control[@"request"], 64);
    lastControlSequence = sequence;
    lastControl = [control copy];
    return control;
}

@interface HeldResponse : NSObject
@property(nonatomic) NSUInteger request;
@property(nonatomic, strong) id<IMKTextInput> sender;
@property(nonatomic, copy) NSString *preedit;
@property(nonatomic, copy) NSString *commit;
@property(nonatomic) BOOL finishedMessageObserved;
@end
@implementation HeldResponse
@end
static NSMutableArray<HeldResponse *> *held;

static NSDictionary *clientInfo(id<IMKTextInput> sender) {
    return @{@"sender":pointer(sender), @"client_identifier":[sender uniqueClientIdentifierString] ?: @"UNAVAILABLE"};
}

static void stopServer(void) {
    [NSApp stop:nil];
    NSEvent *event = [NSEvent otherEventWithType:NSEventTypeApplicationDefined location:NSZeroPoint modifierFlags:0
        timestamp:0 windowNumber:0 context:nil subtype:0 data1:0 data2:0];
    if (event) [NSApp postEvent:event atStart:NO];
}

static void fail(NSException *error) {
    serverStatus = 2;
    emit(@{@"event":@"server_error", @"name":error.name, @"reason":error.reason ?: @"unknown"});
    stopServer();
}

static void releaseResponse(NSUInteger request) {
    HeldResponse *pending = nil;
    for (HeldResponse *item in held) if (item.request == request) pending = item;
    require(pending != nil, @"held request not found or already consumed");
    [held removeObject:pending];
    emit(@{@"event":@"old_response_send_begin", @"request":@(request), @"client":clientInfo(pending.sender),
        @"preedit":pending.preedit, @"commit":pending.commit, @"finished_message_observed":@(pending.finishedMessageObserved)});
    @try {
        [pending.sender setMarkedText:pending.preedit selectionRange:NSMakeRange(pending.preedit.length, 0)
            replacementRange:NSMakeRange(NSNotFound, 0)];
        emit(@{@"event":@"old_preedit_send_return", @"request":@(request)});
        [pending.sender insertText:pending.commit replacementRange:NSMakeRange(NSNotFound, 0)];
        emit(@{@"event":@"old_commit_send_return", @"request":@(request), @"return_is_not_callback_ack":@YES});
    } @catch (NSException *error) {
        emit(@{@"event":@"old_response_send_exception", @"request":@(request), @"name":error.name,
            @"effect":@"UNKNOWN_NO_RETRY_NOT_SAFE_REJECTION"});
        @throw;
    }
}

@interface S2DCIAsyncController : IMKInputController
@property(nonatomic, copy) NSString *activeMarked;
@end
@implementation S2DCIAsyncController
- (BOOL)inputText:(NSString *)string key:(NSInteger)keyCode modifiers:(NSUInteger)flags client:(id)sender {
    inputDepth++;
    @try {
        if (![[sender bundleIdentifier] isEqual:expectedClientBundle]) {
            emit(@{@"event":@"unexpected_client_refused"});
            return NO;
        }
        require(NSThread.isMainThread, @"IME input off main thread");
        require(++requestSequence <= 64, @"request limit exceeded");
        NSUInteger request = requestSequence;
        emit(@{@"event":@"input_received", @"request":@(request), @"keycode":@(keyCode), @"modifiers":@(flags),
            @"text":[string copy], @"controller":pointer(self), @"client":clientInfo(sender)});
        if (keyCode == 0) {
            self.activeMarked = [NSString stringWithFormat:@"active-%lu", (unsigned long)request];
            [sender setMarkedText:self.activeMarked selectionRange:NSMakeRange(self.activeMarked.length, 0)
                replacementRange:NSMakeRange(NSNotFound, 0)];
        } else if (keyCode == 1) {
            require(self.activeMarked.length != 0, @"tail requested without initial composition");
            HeldResponse *pending = [HeldResponse new];
            pending.request = request; pending.sender = sender;
            pending.preedit = [NSString stringWithFormat:@"retired-preedit-%lu", (unsigned long)request];
            pending.commit = [NSString stringWithFormat:@"retired-commit-%lu", (unsigned long)request];
            [held addObject:pending];
            emit(@{@"event":@"response_held", @"request":@(request), @"client":clientInfo(sender),
                @"preedit":pending.preedit, @"commit":pending.commit});
        } else if (keyCode == 3) {
            emit(@{@"event":@"empty_client_probe", @"request":@(request), @"client":clientInfo(sender)});
        } else if (keyCode == 14 || keyCode == 34) {
            if (armedRequest) {NSUInteger release = armedRequest; armedRequest = 0; releaseResponse(release);}
            self.activeMarked = [NSString stringWithFormat:@"fresh-%lu", (unsigned long)request];
            [sender setMarkedText:self.activeMarked selectionRange:NSMakeRange(self.activeMarked.length, 0)
                replacementRange:NSMakeRange(NSNotFound, 0)];
            [sender insertText:self.activeMarked replacementRange:NSMakeRange(NSNotFound, 0)];
            emit(@{@"event":@"fresh_response_send_return", @"request":@(request), @"text":self.activeMarked,
                @"return_is_not_callback_ack":@YES});
            self.activeMarked = @"";
        } else {
            emit(@{@"event":@"unsupported_key", @"request":@(request), @"keycode":@(keyCode)});
            return NO;
        }
        emit(@{@"event":@"input_handler_return", @"request":@(request), @"accepted":@YES});
        return YES;
    } @catch (NSException *error) {fail(error); return NO;}
    @finally {inputDepth--;}
}
- (void)activateServer:(id)sender {
    @try {
        [super activateServer:sender];
        if ([[sender bundleIdentifier] isEqual:expectedClientBundle]) emit(@{@"event":@"session_activated", @"controller":pointer(self), @"client":clientInfo(sender)});
    } @catch (NSException *error) {fail(error);}
}
- (void)deactivateServer:(id)sender {
    @try {
        if ([[sender bundleIdentifier] isEqual:expectedClientBundle]) emit(@{@"event":@"session_deactivated", @"controller":pointer(self), @"client":clientInfo(sender)});
        [super deactivateServer:sender];
    } @catch (NSException *error) {fail(error);}
}
- (void)commitComposition:(id)sender {
    @try {
        self.activeMarked = @"";
        for (HeldResponse *pending in held) if (pending.sender == sender) pending.finishedMessageObserved = YES;
        if ([[sender bundleIdentifier] isEqual:expectedClientBundle]) emit(@{@"event":@"composition_finished", @"controller":pointer(self), @"client":clientInfo(sender)});
    } @catch (NSException *error) {fail(error);}
}
- (id)composedString:(id)sender {(void)sender; return self.activeMarked ?: @"";}
- (NSUInteger)recognizedEvents:(id)sender {(void)sender; return NSEventMaskKeyDown;}
@end

int main(int argc, const char *argv[]) {
    if (argc != 3) return 64;
#if !defined(__arm64__)
    fputs("ARM64-only IME fixture\n", stderr); return 77;
#endif
    if (!getenv("GITHUB_ACTIONS") || strcmp(getenv("GITHUB_ACTIONS"), "true") ||
        !getenv("RUNNER_OS") || strcmp(getenv("RUNNER_OS"), "macOS") ||
        !getenv("RUNNER_ENVIRONMENT") || strcmp(getenv("RUNNER_ENVIRONMENT"), "github-hosted")) {
        fputs("Hosted-CI-only IME server; local execution disabled\n", stderr); return 77;
    }
    @autoreleasepool {
        controlDirectory = [NSString stringWithUTF8String:argv[1]];
        expectedClientBundle = [NSString stringWithUTF8String:argv[2]];
        held = [NSMutableArray new];
        @try {
            require([expectedClientBundle hasPrefix:@"com.geekjapan.soul2d.ci.client."], @"unexpected client bundle identifier");
            [NSApplication sharedApplication];
            require([NSApp setActivationPolicy:NSApplicationActivationPolicyProhibited], @"cannot set owned background server policy");
            [NSApp finishLaunching];
            NSBundle *bundle = NSBundle.mainBundle;
            NSString *connection = bundle.infoDictionary[@"InputMethodConnectionName"];
            require(connection.length != 0 && [bundle.bundleIdentifier hasPrefix:@"com.geekjapan.soul2d.ci.ime."], @"generated app metadata required");
            __attribute__((objc_precise_lifetime)) IMKServer *server = [[IMKServer alloc] initWithName:connection bundleIdentifier:bundle.bundleIdentifier];
            require(server != nil, @"IMKServer initialization failed");
            emit(@{@"event":@"server_ready", @"bundle":bundle.bundleIdentifier, @"connection":connection,
                @"scope":@"OWNED_INPUT_METHOD_SESSION_RESPONSE_PROBE_NOT_PRODUCT_ACCEPTANCE"});
            __attribute__((objc_precise_lifetime)) NSTimer *timer = [NSTimer scheduledTimerWithTimeInterval:0.01 repeats:YES block:^(NSTimer *unused) {
                (void)unused;
                @try {
                    NSString *path = [controlDirectory stringByAppendingPathComponent:@"control.json"];
                    NSDictionary *attributes = [NSFileManager.defaultManager attributesOfItemAtPath:path error:nil];
                    if (!attributes) return;
                    require([attributes[NSFileSize] unsignedLongLongValue] <= 16384, @"control file exceeds limit");
                    NSData *data = [NSData dataWithContentsOfFile:path];
                    require(data != nil && data.length <= 16384, @"control read failed or exceeds limit");
                    NSDictionary *control = acceptControl([NSJSONSerialization JSONObjectWithData:data options:0 error:nil]);
                    if (!control) return;
                    require(inputDepth == 0, @"control timer reentered an input handler");
                    emit(@{@"event":@"control_accepted", @"control":control});
                    if ([control[@"operation"] isEqual:@"stop"]) {stopServer(); return;}
                    NSUInteger request = number(control[@"request"], 64);
                    if ([control[@"operation"] isEqual:@"release"]) releaseResponse(request);
                    else {require(armedRequest == 0, @"release already armed"); armedRequest = request;}
                    emit(@{@"event":@"control_applied", @"control":control});
                } @catch (NSException *error) {fail(error);}
            }];
            [NSApp run];
            [timer invalidate];
            emit(@{@"event":@"server_stopped", @"held_responses_remaining":@(held.count), @"status":@(serverStatus)});
        } @catch (NSException *error) {serverStatus = 2; emit(@{@"event":@"server_start_failed", @"name":error.name, @"reason":error.reason ?: @"unknown"});}
        return serverStatus;
    }
}
