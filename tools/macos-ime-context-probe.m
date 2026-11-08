#import <Cocoa/Cocoa.h>
#import <CoreGraphics/CoreGraphics.h>
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
static NSMutableArray<NSDictionary *> *ownerStates;
static NSMutableArray<id> *clientsRetainedToProcessExit;
static NSMutableDictionary<NSString *, NSString *> *committed;
static NSMutableDictionary<NSString *, NSString *> *preedit;
static NSUInteger consumed;
static BOOL finished;
static CGEventSourceRef keyboardSource;

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

static NSDictionary *observeOwnerState(NSView *view, NSString *boundary) {
    NSDictionary *state = @{@"boundary":boundary, @"index":@(ownerStates.count),
        @"first_responder":pointer(view.window.firstResponder),
        @"context":pointer(NSTextInputContext.currentInputContext),
        @"context_client":pointer(NSTextInputContext.currentInputContext.client)};
    [ownerStates addObject:state];
    return state;
}

@interface Client : NSView <NSTextInputClient>
@property(nonatomic, readonly, copy) NSString *identity;
@property(nonatomic, copy) NSString *marked;
@property(nonatomic, weak) NSView *view;
- (instancetype)initWithIdentity:(NSString *)identity view:(NSView *)view;
@end

@implementation Client
- (instancetype)initWithIdentity:(NSString *)identity view:(NSView *)view {
    if ((self = [super initWithFrame:view.bounds])) {
        _identity = [identity copy]; _marked = @""; _view = self;
        self.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
        [view addSubview:self];
    }
    return self;
}
- (BOOL)acceptsFirstResponder {return YES;}
- (void)keyDown:(NSEvent *)event {
    NSString *previous = api;
    NSTextInputContext *ownerContext = self.inputContext;
    NSDictionary *entry = observeOwnerState(self, @"keyDown_enter");
    emit(@{@"event":@"owner_key_down_enter", @"receiver":pointer(self), @"owner_identity":self.identity,
        @"first_responder":entry[@"first_responder"], @"owner_context":pointer(ownerContext),
        @"current_context":entry[@"context"], @"owner_state":entry});
    api = @"interpretKeyEvents";
    @try {
        [self interpretKeyEvents:@[event]];
    } @finally {
        api = previous;
        NSDictionary *exit = observeOwnerState(self, @"keyDown_return");
        emit(@{@"event":@"owner_key_down_return", @"receiver":pointer(self),
            @"first_responder":exit[@"first_responder"], @"current_context":exit[@"context"], @"owner_state":exit});
    }
}
- (NSString *)plain:(id)value {
    if ([value isKindOfClass:NSAttributedString.class]) return [value string];
    require([value isKindOfClass:NSString.class], @"unexpected native text object");
    return value;
}
- (void)record:(NSString *)kind text:(NSString *)text range:(NSRange)range {
    require(NSThread.isMainThread, @"text callback off main thread");
    NSString *origin = [mode isEqual:@"shared"] ? activeOwner : self.identity;
    NSDictionary *state = observeOwnerState(self, @"native_callback");
    NSDictionary *event = @{@"kind":kind, @"text":[text copy], @"origin":origin,
        @"native_text_object":pointer(text), @"native_text_mutable":@([text isKindOfClass:NSMutableString.class]),
        @"receiver":pointer(self), @"receiver_identity":self.identity,
        @"origin_kind":[mode isEqual:@"shared"] ? @"MUTABLE_CALLBACK_SNAPSHOT" : @"IMMUTABLE_CLIENT",
        @"context":state[@"context"], @"context_client":state[@"context_client"],
        @"first_responder":state[@"first_responder"], @"owner_state":state,
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

static Session *session(NSString *owner, NSView *view, Session *shared) {
    Session *result = [Session new];
    result.owner = owner;
    result.client = shared ? shared.client : [[Client alloc] initWithIdentity:[mode isEqual:@"shared"] ? @"shared" : owner view:view];
    if (!shared) [clientsRetainedToProcessExit addObject:result.client];
    result.context = shared ? shared.context : result.client.inputContext;
    require(result.context != nil && result.context.client == result.client, @"view-owned context client mismatch");
    require(result.client.window == view.window, @"owner view is not attached to owned window");
    emit(@{@"event":@"session_created", @"owner":owner, @"context":pointer(result.context), @"client":pointer(result.client)});
    return result;
}

static void selectOwner(Session *next, Session *old, NSView *view) {
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
    require([view.window makeFirstResponder:next.client], @"cannot focus actual owner view");
    api = @"activate_new";
    [next.context activate];
    api = @"idle";
    emit(@{@"event":@"context_selected", @"owner":next.owner, @"context":pointer(next.context),
        @"current_context":pointer(NSTextInputContext.currentInputContext),
        @"client":pointer(next.context.client), @"first_responder":pointer(view.window.firstResponder)});
    require(NSTextInputContext.currentInputContext == next.context, @"activation did not select expected context");
    require(view.window.isKeyWindow && view.window.firstResponder == next.client, @"owned native owner view lacks focus");
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

static BOOL ownerStatePreservedSince(NSUInteger start, Session *owner) {
    if (start >= ownerStates.count) return NO;
    for (NSUInteger i = start; i < ownerStates.count; i++) {
        NSDictionary *state = ownerStates[i];
        if (![state[@"first_responder"] isEqual:pointer(owner.client)]
            || ![state[@"context"] isEqual:pointer(owner.context)]
            || ![state[@"context_client"] isEqual:pointer(owner.client)]) return NO;
    }
    return YES;
}

static BOOL submitKey(Session *target, CGKeyCode code, BOOL down, CGEventFlags flags) {
    CGEventRef backing = CGEventCreateKeyboardEvent(keyboardSource, code, down);
    require(backing != NULL, @"owned Quartz key construction failed");
    @try {
        CGEventSetFlags(backing, flags);
        NSEvent *event = [NSEvent eventWithCGEvent:backing];
        require(event != nil, @"Quartz to AppKit event conversion failed");
        BOOL textEvent = event.type == NSEventTypeKeyDown || event.type == NSEventTypeKeyUp;
        UniCharCount count = 0;
        UniChar text[32];
        if (textEvent) CGEventKeyboardGetUnicodeString(backing, 32, &count, text);
        require(count <= 32, @"unexpected oversized generated key text");
        emit(@{@"event":@"native_key_enter", @"requested_owner":target.owner, @"keycode":@(event.keyCode),
            @"type":@(event.type), @"flags":@(event.modifierFlags), @"window":@(event.windowNumber),
            @"characters":textEvent ? (event.characters ?: @"") : @"NOT_KEY_TEXT_EVENT",
            @"unmodified":textEvent ? (event.charactersIgnoringModifiers ?: @"") : @"NOT_KEY_TEXT_EVENT",
            @"unicode_payload":[NSString stringWithCharacters:text length:count],
            @"source_state_id":@(CGEventGetIntegerValueField(backing, kCGEventSourceStateID)),
            @"has_backing_event":@(event.CGEvent != NULL), @"posted":@NO});
        BOOL submitted = event.type == NSEventTypeKeyDown;
        if (submitted) [target.client keyDown:event];
        emit(@{@"event":@"native_key_return", @"submitted_to_responder":@(submitted),
            @"entrypoint":@"OWNER_VIEW_KEYDOWN_INTERPRET_KEY_EVENTS",
            @"current_context":pointer(NSTextInputContext.currentInputContext),
            @"current_client":pointer(NSTextInputContext.currentInputContext.client)});
        return submitted;
    } @finally {
        CFRelease(backing);
    }
}

static BOOL key(Session *target, NSString *characters, NSString *unmodified, unsigned short code, NSEventModifierFlags flags) {
    require(flags == 0 || flags == NSEventModifierFlagOption, @"unsupported stimulus modifier");
    invocation = ++nextInvocation;
    api = @"keyDown";
    emit(@{@"event":@"key_enter", @"requested_owner":target.owner, @"context":pointer(target.context),
        @"requested_characters":characters, @"requested_unmodified":unmodified, @"keycode":@(code), @"flags":@(flags)});
    BOOL option = flags == NSEventModifierFlagOption;
    CGEventFlags nativeFlags = option ? kCGEventFlagMaskAlternate : 0;
    BOOL dispatched = NO;
    if (option) dispatched |= submitKey(target, 58, YES, nativeFlags);
    dispatched |= submitKey(target, code, YES, nativeFlags);
    dispatched |= submitKey(target, code, NO, nativeFlags);
    if (option) dispatched |= submitKey(target, 58, NO, 0);
    invocation = 0;
    api = @"idle";
    return dispatched;
}

static void awaitCallback(NSUInteger start, NSString *kind, NSString *owner) {
    NSTimeInterval deadline = NSProcessInfo.processInfo.systemUptime + 2.0;
    while (!countSince(start, kind, owner) && NSProcessInfo.processInfo.systemUptime < deadline) pump();
    require(countSince(start, kind, owner) != 0, [@"STIMULUS_INCONCLUSIVE missing native " stringByAppendingString:kind]);
}

static void compose(Session *target, NSString *letter, unsigned short code, NSString *expected, BOOL deliver) {
    NSUInteger start = callbacks.count;
    key(target, @"", @"e", 14, NSEventModifierFlagOption);
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
    if (argc != 2 || (strcmp(argv[1], "qualify") && strcmp(argv[1], "shared") && strcmp(argv[1], "distinct"))) return 64;
    if (!getenv("GITHUB_ACTIONS") || strcmp(getenv("GITHUB_ACTIONS"), "true") ||
        !getenv("RUNNER_OS") || strcmp(getenv("RUNNER_OS"), "macOS")) {
        fputs("CI-only native fixture; local GUI execution is disabled\n", stderr);
        return 77;
    }
    @autoreleasepool {
        mode = [NSString stringWithUTF8String:argv[1]];
        callbacks = [NSMutableArray new]; ownerStates = [NSMutableArray new];
        committed = [NSMutableDictionary new]; preedit = [NSMutableDictionary new];
        clientsRetainedToProcessExit = [NSMutableArray new];
        __attribute__((objc_precise_lifetime)) NSMutableArray<Session *> *owners = [NSMutableArray new];
        NSWindow *window = nil;
        int status = 2;
        @try {
            [NSApplication sharedApplication];
            require([NSApp setActivationPolicy:NSApplicationActivationPolicyRegular], @"cannot set owned app activation policy");
            [NSApp finishLaunching];
            keyboardSource = CGEventSourceCreate(kCGEventSourceStatePrivate);
            require(keyboardSource != NULL, @"owned private keyboard source creation failed");
            window = [[NSWindow alloc] initWithContentRect:NSMakeRect(100, 100, 640, 360)
                styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable backing:NSBackingStoreBuffered defer:NO];
            window.releasedWhenClosed = NO;
            window.title = @"Soul2D native context prototype";
            NSView *view = [[NSView alloc] initWithFrame:window.contentView.bounds];
            window.contentView = view;
            [window makeKeyAndOrderFront:nil]; [NSApp activateIgnoringOtherApps:YES];
            NSTimeInterval focusDeadline = NSProcessInfo.processInfo.systemUptime + 5.0;
            while (!window.isKeyWindow && NSProcessInfo.processInfo.systemUptime < focusDeadline) pump();
            require(window.isKeyWindow, @"native window did not become key");
            emit(@{@"event":@"environment", @"mode":mode, @"window":@(window.windowNumber),
                @"scope":@"NATIVE_OWNER_VIEW_AND_APPLICATION_QUEUE_PROTOTYPE_NOT_PRODUCT_ACCEPTANCE"});
            Session *a = session(@"A", view, nil); [owners addObject:a];
            Session *b = session(@"B", view, [mode isEqual:@"shared"] ? a : nil); [owners addObject:b];
            Session *s = session(@"sibling", view, [mode isEqual:@"shared"] ? a : nil); [owners addObject:s];
            stage = @"queue_old_native_callbacks"; selectOwner(a, nil, view);
            emit(@{@"event":@"selected_input_source", @"identifier":a.context.selectedKeyboardInputSource ?: @"UNKNOWN"});
            compose(a, @"a", 0, @"á", NO);
            if ([mode isEqual:@"qualify"]) {
                consume();
                require([projection(a.owner) isEqual:@"á"], @"qualified native commit differs");
                emit(@{@"event":@"stimulus_qualified", @"result":@"NATIVE_NONEMPTY_PREEDIT_AND_COMPOSED_COMMIT",
                    @"scope":@"SINGLE_OWNER_STIMULUS_ONLY_NOT_OWNERSHIP_OR_PRODUCT_ACCEPTANCE"});
                finished = YES;
                return 0;
            }
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
            stage = @"retained_view_challenge";
            Session *old = session(@"late-A", view, nil); [owners addObject:old];
            Session *current = session(@"late-B", view, [mode isEqual:@"shared"] ? old : nil); [owners addObject:current];
            selectOwner(old, s, view);
            NSUInteger start = callbacks.count;
            key(old, @"", @"e", 14, NSEventModifierFlagOption); awaitCallback(start, @"preedit", old.owner); consume();
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
            NSUInteger beforeChallengeStates = ownerStates.count;
            emit(@{@"event":@"retained_view_challenge_entry", @"owner_state":observeOwnerState(current.client, @"challenge_entry")});
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
            BOOL statePreserved = ownerStatePreservedSince(beforeChallengeStates, current)
                && window.firstResponder == current.client && NSTextInputContext.currentInputContext == current.context;
            emit(@{@"event":@"retained_context_observation", @"same_context_as_current":@(old.context == current.context),
                @"callbacks":@(callbacks.count - challengeStart), @"new_owner_value":projection(current.owner),
                @"nonempty_text_callbacks":@(meaningful.count),
                @"first_responder":pointer(window.firstResponder), @"current_context":pointer(NSTextInputContext.currentInputContext),
                @"expected_current_client":pointer(current.client), @"expected_current_context":pointer(current.context),
                @"owner_state_samples":@(ownerStates.count - beforeChallengeStates), @"current_state_preserved":@(statePreserved),
                @"scope":@"EXPLICIT_RETAINED_VIEW_KEYDOWN_NOT_QUEUED_OS_REPLY"});
            if ([mode isEqual:@"distinct"] && (!statePreserved || ![projection(current.owner) isEqual:@""])) {
                @throw [NSException exceptionWithName:@"CandidateRefuted"
                    reason:@"retained view changed current owner text or observed focus/context, even if later restored or silent" userInfo:nil];
            }
            require(meaningful.count != 0, @"INCONCLUSIVE no nonempty callback from retained-view invocation");
            if ([mode isEqual:@"distinct"]) {
                BOOL preserved = YES;
                for (NSDictionary *event in meaningful) {
                    preserved = preserved && [event[@"receiver"] isEqual:pointer(old.client)]
                        && [event[@"origin"] isEqual:old.owner] && [event[@"origin_kind"] isEqual:@"IMMUTABLE_CLIENT"]
                        && [event[@"context"] isEqual:pointer(current.context)]
                        && [event[@"context_client"] isEqual:pointer(current.client)]
                        && [event[@"first_responder"] isEqual:pointer(current.client)];
                }
                if (!preserved) @throw [NSException exceptionWithName:@"CandidateRefuted"
                    reason:@"retained owner-view dispatch did not preserve old receiver and empty focused current owner" userInfo:nil];
                stage = @"fresh_B_after_retained_view";
                compose(current, @"e", 14, @"é", YES);
                require([projection(current.owner) isEqual:@"é"], @"immediate B after retained-view challenge differs");
                stage = @"sibling_after_retained_view";
                selectOwner(s, current, view); compose(s, @"i", 34, @"í", YES);
                require([projection(s.owner) isEqual:@"í"] && [projection(current.owner) isEqual:@"é"],
                    @"sibling after retained-view challenge or B preservation failed");
            }
            stage = @"complete";
            emit(@{@"event":@"outcome", @"result":[mode isEqual:@"distinct"] ? @"BOUNDED_OWNER_VIEW_QUEUE_FEASIBLE" : @"SHARED_BASELINE_OBSERVED",
                @"retained_context_nonempty_callbacks":@(meaningful.count),
                @"delayed_OS_reply_isolation":@"NOT_PROVED", @"physical_IME":@"NOT_RUN", @"pinned_winit_integration":@"NOT_RUN",
                @"entrypoint":@"OWNER_VIEW_KEYDOWN_INTERPRET_KEY_EVENTS_NOT_PINNED_WINIT_INTEGRATION"});
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
            if (keyboardSource) {CFRelease(keyboardSource); keyboardSource = NULL;}
            emit(@{@"event":@"owned_window_closed", @"experiment_complete":@(finished),
                @"window_created":@(window != nil), @"visible_after_close":@(window.isVisible),
                @"owner_sessions_retained_through_cleanup":@(owners.count),
                @"clients_retained_to_process_exit":@(clientsRetainedToProcessExit.count),
                @"owned_keyboard_source_released":@(keyboardSource == NULL),
                @"unique_contexts_closed":@(closedContexts.count), @"callbacks_observed":@(callbacks.count),
                @"callbacks_consumed_before_cleanup":@(consumed)});
        }
        return status;
    }
}
