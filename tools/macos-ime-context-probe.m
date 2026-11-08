#import <Cocoa/Cocoa.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static NSString *mode;
static NSString *stage = @"startup";
static NSString *activeOwner = @"none";
static NSString *api = @"idle";
static NSUInteger serial;
static NSUInteger invocation;
static NSUInteger nextInvocation;
static NSMutableArray<NSDictionary *> *callbacks;
static NSMutableArray<id> *clientsRetainedToProcessExit;
static NSMutableDictionary<NSString *, NSString *> *committed;
static NSMutableDictionary<NSString *, NSString *> *preedit;
static NSUInteger consumed;
static BOOL finished;

static NSString *pointer(id object) {
    return [NSString stringWithFormat:@"%p", (__bridge void *)object];
}

static void emit(NSDictionary *fields) {
    if (++serial > 4096) @throw [NSException exceptionWithName:@"TraceLimit" reason:@"callback trace exceeded limit" userInfo:nil];
    NSMutableDictionary *row = [fields mutableCopy];
    row[@"seq"] = @(serial);
    row[@"stage"] = stage;
    row[@"api"] = api;
    row[@"invocation"] = @(invocation);
    row[@"active_owner"] = activeOwner;
    NSData *data = [NSJSONSerialization dataWithJSONObject:row options:NSJSONWritingSortedKeys error:nil];
    if (!data || fwrite(data.bytes, 1, data.length, stdout) != data.length || fputc('\n', stdout) == EOF) {
        @throw [NSException exceptionWithName:@"TraceWrite" reason:@"cannot write native trace" userInfo:nil];
    }
    fflush(stdout);
}

static void require(BOOL condition, NSString *reason) {
    if (!condition) @throw [NSException exceptionWithName:@"ProbeIncomplete" reason:reason userInfo:nil];
}

@interface Client : NSObject <NSTextInputClient>
@property(nonatomic, readonly, copy) NSString *identity;
@property(nonatomic, copy) NSString *marked;
@property(nonatomic, weak) NSView *view;
- (instancetype)initWithIdentity:(NSString *)identity view:(NSView *)view;
@end

@implementation Client
- (instancetype)initWithIdentity:(NSString *)identity view:(NSView *)view {
    if ((self = [super init])) {_identity = [identity copy]; _marked = @""; _view = view;}
    return self;
}
- (NSString *)plain:(id)value {
    if ([value isKindOfClass:NSAttributedString.class]) return [value string];
    require([value isKindOfClass:NSString.class], @"unexpected native text object");
    return value;
}
- (void)record:(NSString *)kind text:(NSString *)text range:(NSRange)range {
    require(NSThread.isMainThread, @"text callback off main thread");
    NSString *origin = [mode isEqual:@"shared"] ? activeOwner : self.identity;
    NSDictionary *event = @{@"kind":kind, @"text":text, @"origin":origin,
        @"receiver":pointer(self), @"receiver_identity":self.identity,
        @"origin_kind":[mode isEqual:@"shared"] ? @"MUTABLE_CALLBACK_SNAPSHOT" : @"IMMUTABLE_CLIENT",
        @"context":pointer(NSTextInputContext.currentInputContext),
        @"context_client":pointer(NSTextInputContext.currentInputContext.client),
        @"range":NSStringFromRange(range), @"api_at_callback":api, @"call_at_callback":@(invocation)};
    [callbacks addObject:event];
    emit(@{@"event":@"native_callback", @"callback":event, @"index":@(callbacks.count - 1)});
}
- (void)insertText:(id)value replacementRange:(NSRange)range {
    self.marked = @"";
    [self record:@"commit" text:[self plain:value] range:range];
}
- (void)setMarkedText:(id)value selectedRange:(NSRange)selected replacementRange:(NSRange)replacement {
    self.marked = [self plain:value];
    [self record:@"preedit" text:self.marked range:replacement];
    emit(@{@"event":@"native_selection", @"receiver":pointer(self), @"selected":NSStringFromRange(selected)});
}
- (void)unmarkText {
    self.marked = @"";
    [self record:@"unmark" text:@"" range:NSMakeRange(NSNotFound, 0)];
}
- (void)doCommandBySelector:(SEL)selector {
    [self record:@"command" text:NSStringFromSelector(selector) range:NSMakeRange(NSNotFound, 0)];
}
- (BOOL)hasMarkedText {return self.marked.length != 0;}
- (NSRange)markedRange {return self.marked.length ? NSMakeRange(0, self.marked.length) : NSMakeRange(NSNotFound, 0);}
- (NSRange)selectedRange {return NSMakeRange(NSNotFound, 0);}
- (NSArray<NSAttributedStringKey> *)validAttributesForMarkedText {return @[];}
- (NSAttributedString *)attributedSubstringForProposedRange:(NSRange)range actualRange:(NSRangePointer)actual {
    (void)range;
    if (actual) *actual = NSMakeRange(NSNotFound, 0);
    return nil;
}
- (NSUInteger)characterIndexForPoint:(NSPoint)point {(void)point; return 0;}
- (NSRect)firstRectForCharacterRange:(NSRange)range actualRange:(NSRangePointer)actual {
    if (actual) *actual = range;
    return [self.view.window convertRectToScreen:[self.view convertRect:NSMakeRect(24, 240, 12, 20) toView:nil]];
}
- (NSInteger)windowLevel {return self.view.window.level;}
@end

@interface Canvas : NSView
@property(nonatomic, strong) NSTextInputContext *selectedContext;
@end
@implementation Canvas
- (BOOL)acceptsFirstResponder {return YES;}
- (NSTextInputContext *)inputContext {return self.selectedContext;}
@end

@interface Session : NSObject
@property(nonatomic, copy) NSString *owner;
@property(nonatomic, strong) Client *client;
@property(nonatomic, strong) NSTextInputContext *context;
@end
@implementation Session
@end

static void pump(void) {
    NSString *previous = api;
    api = @"event_loop_observation";
    NSEvent *event = [NSApp nextEventMatchingMask:NSEventMaskAny untilDate:[NSDate dateWithTimeIntervalSinceNow:0.01]
        inMode:NSDefaultRunLoopMode dequeue:YES];
    if (event) [NSApp sendEvent:event];
    [NSApp updateWindows];
    api = previous;
}

static Session *session(NSString *owner, Canvas *view, Session *shared) {
    Session *result = [Session new];
    result.owner = owner;
    result.client = shared ? shared.client : [[Client alloc] initWithIdentity:[mode isEqual:@"shared"] ? @"shared" : owner view:view];
    if (!shared) [clientsRetainedToProcessExit addObject:result.client];
    result.context = shared ? shared.context : [[NSTextInputContext alloc] initWithClient:result.client];
    require(result.context.client == result.client, @"context client mismatch");
    emit(@{@"event":@"session_created", @"owner":owner, @"context":pointer(result.context), @"client":pointer(result.client)});
    return result;
}

static void selectOwner(Session *next, Session *old, Canvas *view) {
    activeOwner = next.owner;
    committed[next.owner] = @"";
    preedit[next.owner] = @"";
    emit(@{@"event":@"logical_retirement", @"old_owner":old ? old.owner : @"none", @"new_owner":next.owner});
    if (old && old.context != next.context) {
        api = @"discard_old";
        old.client.marked = @"";
        [old.context discardMarkedText];
        api = @"deactivate_old";
        [old.context deactivate];
    } else if (old) {
        old.client.marked = @"";
        emit(@{@"event":@"shared_local_marked_clear", @"context":pointer(old.context)});
    }
    view.selectedContext = next.context;
    api = @"activate_new";
    [next.context activate];
    api = @"idle";
    emit(@{@"event":@"context_selected", @"owner":next.owner, @"context":pointer(next.context),
        @"current_context":pointer(NSTextInputContext.currentInputContext),
        @"client":pointer(next.context.client), @"first_responder":pointer(view.window.firstResponder)});
    require(NSTextInputContext.currentInputContext == next.context, @"activation did not select expected context");
    require(view.window.isKeyWindow && view.window.firstResponder == view, @"owned native window lacks focus");
}

static NSString *projection(NSString *owner) {
    return [NSString stringWithFormat:@"%@%@", committed[owner] ?: @"", preedit[owner] ?: @""];
}

static void consume(void) {
    while (consumed < callbacks.count) {
        NSDictionary *event = callbacks[consumed];
        BOOL accepted = [event[@"origin"] isEqual:activeOwner];
        if (accepted) {
            if ([event[@"kind"] isEqual:@"preedit"]) preedit[activeOwner] = event[@"text"];
            if ([event[@"kind"] isEqual:@"commit"]) {
                committed[activeOwner] = [(committed[activeOwner] ?: @"") stringByAppendingString:event[@"text"]];
                preedit[activeOwner] = @"";
            }
            if ([event[@"kind"] isEqual:@"unmark"]) preedit[activeOwner] = @"";
        }
        emit(@{@"event":@"application_admission", @"index":@(consumed++), @"origin":event[@"origin"],
            @"accepted":@(accepted), @"projected_value":projection(activeOwner)});
    }
}

static NSUInteger countSince(NSUInteger start, NSString *kind, NSString *owner) {
    NSUInteger count = 0;
    for (NSUInteger i = start; i < callbacks.count; i++) {
        NSDictionary *event = callbacks[i];
        if ([event[@"kind"] isEqual:kind] && [event[@"origin"] isEqual:owner] && [event[@"text"] length]) count++;
    }
    return count;
}

static BOOL ownerTextSince(NSUInteger start, Session *owner) {
    for (NSUInteger i = start; i < callbacks.count; i++) {
        NSDictionary *event = callbacks[i];
        BOOL text = ([event[@"kind"] isEqual:@"preedit"] || [event[@"kind"] isEqual:@"commit"])
            && [event[@"text"] length] != 0;
        if (text && ([event[@"origin"] isEqual:owner.owner] || [event[@"receiver"] isEqual:pointer(owner.client)])) return YES;
    }
    return NO;
}

static BOOL key(Session *target, NSString *characters, NSString *unmodified, unsigned short code, NSEventModifierFlags flags) {
    invocation = ++nextInvocation;
    api = @"handleEvent";
    emit(@{@"event":@"key_enter", @"requested_owner":target.owner, @"context":pointer(target.context),
        @"characters":characters, @"keycode":@(code), @"flags":@(flags)});
    NSEvent *event = [NSEvent keyEventWithType:NSEventTypeKeyDown location:NSZeroPoint modifierFlags:flags
        timestamp:NSProcessInfo.processInfo.systemUptime windowNumber:target.client.view.window.windowNumber
        context:nil characters:characters charactersIgnoringModifiers:unmodified isARepeat:NO keyCode:code];
    require(event != nil, @"native key construction failed");
    BOOL handled = [target.context handleEvent:event];
    emit(@{@"event":@"key_return", @"handled":@(handled),
        @"current_context":pointer(NSTextInputContext.currentInputContext),
        @"current_client":pointer(NSTextInputContext.currentInputContext.client)});
    invocation = 0;
    api = @"idle";
    return handled;
}

static void awaitCallback(NSUInteger start, NSString *kind, NSString *owner) {
    NSTimeInterval deadline = NSProcessInfo.processInfo.systemUptime + 2.0;
    while (!countSince(start, kind, owner) && NSProcessInfo.processInfo.systemUptime < deadline) pump();
    require(countSince(start, kind, owner) != 0, [@"STIMULUS_INCONCLUSIVE missing native " stringByAppendingString:kind]);
}

static void compose(Session *target, NSString *letter, unsigned short code, NSString *expected, BOOL deliver) {
    NSUInteger start = callbacks.count;
    key(target, @"´", @"e", 14, NSEventModifierFlagOption);
    awaitCallback(start, @"preedit", target.owner);
    if (deliver) {consume(); require(projection(target.owner).length != 0, @"native preedit not delivered");}
    key(target, letter, letter, code, 0);
    awaitCallback(start, @"commit", target.owner);
    BOOL found = NO;
    for (NSUInteger i = start; i < callbacks.count; i++) {
        NSDictionary *event = callbacks[i];
        if ([event[@"origin"] isEqual:target.owner] && [event[@"kind"] isEqual:@"commit"] && [event[@"text"] isEqual:expected]) found = YES;
    }
    require(found, @"STIMULUS_INCONCLUSIVE unexpected composed character");
    if (deliver) consume();
}

int main(int argc, const char *argv[]) {
    if (argc != 2 || (strcmp(argv[1], "shared") && strcmp(argv[1], "distinct"))) return 64;
    if (!getenv("GITHUB_ACTIONS") || strcmp(getenv("GITHUB_ACTIONS"), "true") ||
        !getenv("RUNNER_OS") || strcmp(getenv("RUNNER_OS"), "macOS")) {
        fputs("CI-only native fixture; local GUI execution is disabled\n", stderr);
        return 77;
    }
    @autoreleasepool {
        mode = [NSString stringWithUTF8String:argv[1]];
        callbacks = [NSMutableArray new]; committed = [NSMutableDictionary new]; preedit = [NSMutableDictionary new];
        clientsRetainedToProcessExit = [NSMutableArray new];
        __attribute__((objc_precise_lifetime)) NSMutableArray<Session *> *owners = [NSMutableArray new];
        NSWindow *window = nil;
        int status = 2;
        @try {
            [NSApplication sharedApplication];
            require([NSApp setActivationPolicy:NSApplicationActivationPolicyRegular], @"cannot set owned app activation policy");
            [NSApp finishLaunching];
            window = [[NSWindow alloc] initWithContentRect:NSMakeRect(100, 100, 640, 360)
                styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable backing:NSBackingStoreBuffered defer:NO];
            window.releasedWhenClosed = NO;
            window.title = @"Soul2D native context prototype";
            Canvas *view = [[Canvas alloc] initWithFrame:window.contentView.bounds];
            window.contentView = view;
            [window makeKeyAndOrderFront:nil]; [NSApp activateIgnoringOtherApps:YES];
            require([window makeFirstResponder:view], @"cannot focus owned canvas");
            NSTimeInterval focusDeadline = NSProcessInfo.processInfo.systemUptime + 5.0;
            while (!window.isKeyWindow && NSProcessInfo.processInfo.systemUptime < focusDeadline) pump();
            require(window.isKeyWindow, @"native window did not become key");
            emit(@{@"event":@"environment", @"mode":mode, @"window":@(window.windowNumber),
                @"scope":@"NATIVE_CLIENT_AND_APPLICATION_QUEUE_PROTOTYPE_NOT_PRODUCT_ACCEPTANCE"});
            Session *a = session(@"A", view, nil); [owners addObject:a];
            Session *b = session(@"B", view, [mode isEqual:@"shared"] ? a : nil); [owners addObject:b];
            Session *s = session(@"sibling", view, [mode isEqual:@"shared"] ? a : nil); [owners addObject:s];
            stage = @"queue_old_native_callbacks"; selectOwner(a, nil, view);
            emit(@{@"event":@"selected_input_source", @"identifier":a.context.selectedKeyboardInputSource ?: @"UNKNOWN"});
            compose(a, @"a", 0, @"á", NO);
            NSUInteger beforeFirstRetirement = callbacks.count;
            stage = @"retire_then_release_application_queue"; selectOwner(b, a, view); consume();
            if (ownerTextSince(beforeFirstRetirement, b)) @throw [NSException exceptionWithName:@"CandidateRefuted"
                reason:@"new B received nonempty text before any B input, even if later cleared" userInfo:nil];
            require([projection(@"B") isEqual:@""], @"retired application envelopes changed new B");
            stage = @"immediate_fresh_B"; compose(b, @"e", 14, @"é", YES);
            require([projection(@"B") isEqual:@"é"], @"fresh B projection differs");
            stage = @"sibling"; selectOwner(s, b, view); compose(s, @"i", 34, @"í", YES);
            require([projection(@"sibling") isEqual:@"í"] && [projection(@"B") isEqual:@"é"], @"sibling or B preservation failed");
            emit(@{@"event":@"queue_case_complete", @"retired_entries_rejected":@YES, @"fresh_B":@YES, @"sibling":@YES});
            stage = @"retained_context_challenge";
            Session *old = session(@"late-A", view, nil); [owners addObject:old];
            Session *current = session(@"late-B", view, [mode isEqual:@"shared"] ? old : nil); [owners addObject:current];
            selectOwner(old, s, view);
            NSUInteger start = callbacks.count;
            key(old, @"´", @"e", 14, NSEventModifierFlagOption); awaitCallback(start, @"preedit", old.owner); consume();
            NSUInteger beforeRetirement = callbacks.count;
            selectOwner(current, old, view);
            pump(); consume();
            BOOL currentReceivedText = ownerTextSince(beforeRetirement, current);
            emit(@{@"event":@"post_retirement_observation", @"callbacks":@(callbacks.count - beforeRetirement),
                @"current_received_nonempty_text":@(currentReceivedText),
                @"new_owner_value":projection(current.owner), @"absence_is_not_reset_ack":@YES});
            if ([mode isEqual:@"distinct"] && (currentReceivedText || ![projection(current.owner) isEqual:@""])) {
                @throw [NSException exceptionWithName:@"CandidateRefuted"
                    reason:@"native retirement changed new owner before any resubmission" userInfo:nil];
            }
            NSUInteger challengeStart = callbacks.count;
            key(old, @"o", @"o", 31, 0); pump(); consume();
            NSTimeInterval challengeDeadline = NSProcessInfo.processInfo.systemUptime + 2.0;
            NSMutableArray<NSDictionary *> *meaningful = [NSMutableArray new];
            while (YES) {
                [meaningful removeAllObjects];
                for (NSUInteger i = challengeStart; i < callbacks.count; i++) {
                    NSDictionary *event = callbacks[i];
                    if (([event[@"kind"] isEqual:@"preedit"] || [event[@"kind"] isEqual:@"commit"]) && [event[@"text"] length]) {
                        [meaningful addObject:event];
                    }
                }
                if (meaningful.count || NSProcessInfo.processInfo.systemUptime >= challengeDeadline) break;
                pump(); consume();
            }
            emit(@{@"event":@"retained_context_observation", @"same_context_as_current":@(old.context == current.context),
                @"callbacks":@(callbacks.count - challengeStart), @"new_owner_value":projection(current.owner),
                @"nonempty_text_callbacks":@(meaningful.count),
                @"scope":@"EXPLICIT_RETAINED_CONTEXT_INVOCATION_NOT_QUEUED_OS_REPLY"});
            require(meaningful.count != 0, @"INCONCLUSIVE no nonempty callback from retained-context invocation");
            if ([mode isEqual:@"distinct"]) {
                BOOL preserved = [projection(current.owner) isEqual:@""];
                for (NSDictionary *event in meaningful) {
                    preserved = preserved && [event[@"receiver"] isEqual:pointer(old.client)]
                        && [event[@"origin"] isEqual:old.owner] && [event[@"origin_kind"] isEqual:@"IMMUTABLE_CLIENT"];
                }
                if (!preserved) @throw [NSException exceptionWithName:@"CandidateRefuted"
                    reason:@"retained context did not preserve old immutable receiver and empty current owner" userInfo:nil];
            }
            stage = @"complete";
            emit(@{@"event":@"outcome", @"result":[mode isEqual:@"distinct"] ? @"BOUNDED_NATIVE_CLIENT_QUEUE_FEASIBLE" : @"SHARED_BASELINE_OBSERVED",
                @"retained_context_nonempty_callbacks":@(meaningful.count),
                @"delayed_OS_reply_isolation":@"NOT_PROVED", @"physical_IME":@"NOT_RUN", @"pinned_winit_integration":@"NOT_RUN",
                @"entrypoint":@"PUBLIC_CONTEXT_HANDLE_EVENT_REPLACEMENT_SEAM_NOT_INTERPRET_KEY_EVENTS_EQUIVALENCE"});
            finished = YES; status = 0;
        } @catch (NSException *error) {
            emit(@{@"event":@"incomplete", @"name":error.name, @"reason":error.reason ?: @"unknown"});
            status = [error.name isEqual:@"CandidateRefuted"] ? 3 : 2;
        } @finally {
            stage = @"cleanup"; api = @"cleanup_discard_deactivate";
            NSMutableSet<NSTextInputContext *> *closedContexts = [NSMutableSet new];
            for (Session *owner in owners.reverseObjectEnumerator) {
                if ([closedContexts containsObject:owner.context]) continue;
                [closedContexts addObject:owner.context];
                owner.client.marked = @""; [owner.context discardMarkedText]; [owner.context deactivate];
            }
            [window orderOut:nil]; [window close];
            emit(@{@"event":@"owned_window_closed", @"experiment_complete":@(finished),
                @"window_created":@(window != nil), @"visible_after_close":@(window.isVisible),
                @"owner_sessions_retained_through_cleanup":@(owners.count),
                @"clients_retained_to_process_exit":@(clientsRetainedToProcessExit.count),
                @"unique_contexts_closed":@(closedContexts.count), @"callbacks_observed":@(callbacks.count),
                @"callbacks_consumed_before_cleanup":@(consumed)});
        }
        return status;
    }
}
