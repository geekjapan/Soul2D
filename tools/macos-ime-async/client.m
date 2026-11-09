#import <Cocoa/Cocoa.h>
#import <CoreGraphics/CoreGraphics.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <limits.h>

typedef NS_ENUM(NSUInteger, CasePhase) {Initial, HoldingA, EmptyB, Releasing, FreshB, Sibling, Complete};
static NSString *controlDirectory;
static NSString *serverLog;
static NSString *expectedSource;
static NSString *expectedServerBundle;
static NSString *caseName = @"startup";
static CasePhase phase;
static NSUInteger traceSequence;
static NSUInteger controlSequence;
static NSUInteger dispatchSequence;
static NSUInteger activeDispatch;
static NSMutableArray<NSDictionary *> *callbacks;
static NSMutableArray<NSDictionary *> *states;
static NSMutableArray<NSDictionary *> *challenges;
static CGEventSourceRef keyboardSource;

static void require(BOOL condition, NSString *reason) {
    if (!condition) @throw [NSException exceptionWithName:@"ProbeIncomplete" reason:reason userInfo:nil];
}
static NSString *pointer(id object) {return [NSString stringWithFormat:@"%p", (__bridge void *)object];}
static void emit(NSDictionary *fields) {
    require(++traceSequence <= 4096, @"client trace limit");
    NSMutableDictionary *row = [fields mutableCopy];
    row[@"seq"] = @(traceSequence); row[@"case"] = caseName; row[@"phase"] = @(phase);
    row[@"dispatch"] = @(activeDispatch);
    NSData *data = [NSJSONSerialization dataWithJSONObject:row options:NSJSONWritingSortedKeys error:nil];
    require(data && fwrite(data.bytes, 1, data.length, stdout) == data.length && fputc('\n', stdout) != EOF,
        @"cannot write client trace");
    fflush(stdout);
}
static void observe(NSWindow *window, NSString *boundary) {
    NSDictionary *state = @{@"boundary":boundary, @"first_responder":pointer(window.firstResponder),
        @"context":pointer(NSTextInputContext.currentInputContext),
        @"context_client":pointer(NSTextInputContext.currentInputContext.client)};
    [states addObject:state];
    emit(@{@"event":@"owner_state", @"state":state, @"index":@(states.count - 1)});
}

@interface AsyncClient : NSView <NSTextInputClient>
@property(nonatomic, readonly, copy) NSString *identity;
@property(nonatomic, copy) NSString *marked;
@property(nonatomic, copy) NSString *committed;
- (instancetype)initWithIdentity:(NSString *)identity container:(NSView *)container;
@end
@implementation AsyncClient
- (instancetype)initWithIdentity:(NSString *)identity container:(NSView *)container {
    if ((self = [super initWithFrame:container.bounds])) {
        _identity = [identity copy]; _marked = @""; _committed = @"";
        [container addSubview:self];
    }
    return self;
}
- (BOOL)acceptsFirstResponder {return YES;}
- (void)keyDown:(NSEvent *)event {
    observe(self.window, @"keyDown_enter");
    @try {[self interpretKeyEvents:@[event]];}
    @finally {observe(self.window, @"keyDown_return");}
}
- (NSString *)plain:(id)value {
    if ([value isKindOfClass:NSAttributedString.class]) return [[value string] copy];
    require([value isKindOfClass:NSString.class], @"unexpected native text object");
    return [value copy];
}
- (void)record:(NSString *)kind text:(NSString *)text {
    require(NSThread.isMainThread, @"native callback off main thread");
    NSDictionary *event = @{@"kind":kind, @"text":[text copy], @"receiver":pointer(self),
        @"identity":self.identity, @"dispatch":@(activeDispatch), @"case":caseName};
    [callbacks addObject:event];
    emit(@{@"event":@"native_callback", @"callback":event, @"index":@(callbacks.count - 1)});
    observe(self.window, @"native_callback");
}
- (void)insertText:(id)value replacementRange:(NSRange)range {
    (void)range;
    NSString *text = [self plain:value];
    self.committed = [self.committed stringByAppendingString:text]; self.marked = @"";
    [self record:@"commit" text:text];
}
- (void)setMarkedText:(id)value selectedRange:(NSRange)selected replacementRange:(NSRange)replacement {
    (void)selected; (void)replacement;
    self.marked = [self plain:value];
    [self record:@"preedit" text:self.marked];
}
- (void)unmarkText {self.marked = @""; [self record:@"unmark" text:@""];}
- (void)doCommandBySelector:(SEL)selector {[self record:@"command" text:NSStringFromSelector(selector)];}
- (BOOL)hasMarkedText {return self.marked.length != 0;}
- (NSRange)markedRange {return self.marked.length ? NSMakeRange(0, self.marked.length) : NSMakeRange(NSNotFound, 0);}
- (NSRange)selectedRange {return NSMakeRange(NSNotFound, 0);}
- (NSArray<NSAttributedStringKey> *)validAttributesForMarkedText {return @[];}
- (NSAttributedString *)attributedSubstringForProposedRange:(NSRange)range actualRange:(NSRangePointer)actual {
    (void)range; if (actual) *actual = NSMakeRange(NSNotFound, 0); return nil;
}
- (NSUInteger)characterIndexForPoint:(NSPoint)point {(void)point; return 0;}
- (NSRect)firstRectForCharacterRange:(NSRange)range actualRange:(NSRangePointer)actual {
    if (actual) *actual = range;
    return [self.window convertRectToScreen:[self convertRect:NSMakeRect(24, 240, 12, 20) toView:nil]];
}
- (NSInteger)windowLevel {return self.window.level;}
@end

static NSUInteger positiveInteger(id number, NSUInteger maximum) {
    require([number isKindOfClass:NSNumber.class] && CFGetTypeID((__bridge CFTypeRef)number) != CFBooleanGetTypeID(),
        @"server integer required");
    double scalar = [number doubleValue];
    require(isfinite(scalar) && scalar >= 1 && scalar <= maximum && floor(scalar) == scalar, @"server integer outside bounds");
    return (NSUInteger)scalar;
}
static NSArray<NSDictionary *> *parseServerTrace(NSData *data, NSString *bundle) {
    require(data != nil && data.length <= 1048576, @"server trace missing or oversized");
    NSRange lastNewline = [data rangeOfData:[NSData dataWithBytes:"\n" length:1] options:NSDataSearchBackwards range:NSMakeRange(0, data.length)];
    if (lastNewline.location == NSNotFound) {require(data.length <= 16384, @"oversized partial server row"); return @[];}
    require(data.length - NSMaxRange(lastNewline) <= 16384, @"oversized partial server row");
    NSString *text = [[NSString alloc] initWithData:[data subdataWithRange:NSMakeRange(0, NSMaxRange(lastNewline))] encoding:NSUTF8StringEncoding];
    require(text != nil, @"server trace is not UTF-8");
    NSArray<NSString *> *lines = [text componentsSeparatedByString:@"\n"];
    NSMutableArray<NSDictionary *> *rows = [NSMutableArray new];
    for (NSUInteger i = 0; i + 1 < lines.count; i++) {
        require([lines[i] lengthOfBytesUsingEncoding:NSUTF8StringEncoding] <= 16384, @"oversized server row");
        NSDictionary *row = [NSJSONSerialization JSONObjectWithData:[lines[i] dataUsingEncoding:NSUTF8StringEncoding] options:0 error:nil];
        require([row isKindOfClass:NSDictionary.class] && [row[@"event"] isKindOfClass:NSString.class] &&
            positiveInteger(row[@"seq"], 4096) == rows.count + 1,
            @"malformed or unordered server trace");
        NSUInteger pid = positiveInteger(row[@"pid"], INT_MAX);
        if (rows.count == 0) require([row[@"event"] isEqual:@"server_ready"] && [row[@"bundle"] isEqual:bundle], @"foreign server journal");
        else require(pid == [rows[0][@"pid"] unsignedIntegerValue], @"server process changed");
        require(![row[@"event"] isEqual:@"server_error"] && ![row[@"event"] isEqual:@"server_start_failed"] &&
            ![row[@"event"] isEqual:@"unexpected_client_refused"], @"server failed or received an unexpected client");
        [rows addObject:row];
        require(rows.count <= 4096, @"server trace row limit");
    }
    return rows;
}
static NSArray<NSDictionary *> *serverRows(void) {
    static NSArray<NSDictionary *> *previous;
    NSArray<NSDictionary *> *rows = parseServerTrace([NSData dataWithContentsOfFile:serverLog], expectedServerBundle);
    require(rows.count >= previous.count, @"server journal truncated");
    for (NSUInteger i = 0; i < previous.count; i++) require([rows[i] isEqual:previous[i]], @"server journal history changed");
    previous = rows;
    return rows;
}
static NSDictionary *findServer(NSString *name, NSUInteger request, NSUInteger start) {
    NSArray *rows = serverRows();
    for (NSUInteger i = start; i < rows.count; i++) {
        NSDictionary *row = rows[i];
        if ([row[@"event"] isEqual:name] && (!request || [row[@"request"] unsignedIntegerValue] == request)) return row;
    }
    return nil;
}
static void pump(void) {
    NSEvent *event = [NSApp nextEventMatchingMask:NSEventMaskAny untilDate:[NSDate dateWithTimeIntervalSinceNow:0.01]
        inMode:NSDefaultRunLoopMode dequeue:YES];
    if (event) [NSApp sendEvent:event];
    [NSApp updateWindows];
}
static void waitFor(BOOL (^predicate)(void), NSString *reason) {
    NSTimeInterval deadline = NSProcessInfo.processInfo.systemUptime + 5.0;
    while (!predicate()) {
        require(NSProcessInfo.processInfo.systemUptime < deadline, [@"INCONCLUSIVE " stringByAppendingString:reason]);
        pump();
    }
}
static BOOL callbackFound(NSUInteger start, AsyncClient *client, NSString *kind, NSString *text) {
    for (NSUInteger i = start; i < callbacks.count; i++) {
        NSDictionary *event = callbacks[i];
        if ([event[@"receiver"] isEqual:pointer(client)] && [event[@"kind"] isEqual:kind] && [event[@"text"] isEqual:text]) return YES;
    }
    return NO;
}
static BOOL anyCallbackFound(NSUInteger start, NSString *kind, NSString *text) {
    for (NSUInteger i = start; i < callbacks.count; i++) {
        NSDictionary *event = callbacks[i];
        if ([event[@"kind"] isEqual:kind] && [event[@"text"] isEqual:text]) return YES;
    }
    return NO;
}
static BOOL staleRecord(NSArray<NSDictionary *> *ledger, NSUInteger start, NSString *receiver, NSString *marked, NSString *commit) {
    for (NSUInteger i = start; i < ledger.count; i++) {
        NSDictionary *event = ledger[i];
        if ([event[@"receiver"] isEqual:receiver] &&
            (([event[@"kind"] isEqual:@"preedit"] && [event[@"text"] isEqual:marked]) ||
             ([event[@"kind"] isEqual:@"commit"] && [event[@"text"] isEqual:commit]))) return YES;
    }
    return NO;
}
static BOOL staleAt(NSUInteger start, AsyncClient *client, NSString *marked, NSString *commit) {
    return staleRecord(callbacks, start, pointer(client), marked, commit);
}
static BOOL misroutedRecord(NSArray<NSDictionary *> *ledger, NSUInteger start, NSString *original, NSString *marked, NSString *commit) {
    for (NSUInteger i = start; i < ledger.count; i++) {
        NSDictionary *event = ledger[i];
        BOOL heldPayload = ([event[@"kind"] isEqual:@"preedit"] && [event[@"text"] isEqual:marked]) ||
            ([event[@"kind"] isEqual:@"commit"] && [event[@"text"] isEqual:commit]);
        if (heldPayload && ![event[@"receiver"] isEqual:original]) return YES;
    }
    return NO;
}
static BOOL cumulativeRefutation(void) {
    for (NSDictionary *challenge in challenges) if (misroutedRecord(callbacks, [challenge[@"start"] unsignedIntegerValue],
        challenge[@"original_receiver"], challenge[@"preedit"], challenge[@"commit"])) return YES;
    return NO;
}
static BOOL ownerPreserved(NSUInteger start, AsyncClient *client) {
    if (states.count == start) return NO;
    for (NSUInteger i = start; i < states.count; i++) {
        NSDictionary *state = states[i];
        if (![state[@"first_responder"] isEqual:pointer(client)] ||
            ![state[@"context"] isEqual:pointer(client.inputContext)] ||
            ![state[@"context_client"] isEqual:pointer(client)]) return NO;
    }
    return YES;
}
static NSString *value(AsyncClient *client) {return [client.committed stringByAppendingString:client.marked];}
static void focus(AsyncClient *next, AsyncClient *old) {
    if (old) {[old.inputContext discardMarkedText]; [old.inputContext deactivate];}
    require([next.window makeFirstResponder:next], @"cannot select native responder");
    [next.inputContext activate];
    require(next.inputContext.client == next && next.window.firstResponder == next &&
        next.window.isKeyWindow && NSTextInputContext.currentInputContext == next.inputContext, @"native owner lacks context/focus");
    require([next.inputContext.selectedKeyboardInputSource isEqual:expectedSource], @"owned IME source is not selected");
    observe(next.window, @"focus_selected");
    emit(@{@"event":@"owner_selected", @"identity":next.identity, @"receiver":pointer(next),
        @"context":pointer(next.inputContext), @"source":next.inputContext.selectedKeyboardInputSource ?: @"UNKNOWN"});
}
static NSUInteger key(AsyncClient *client, CGKeyCode code) {
    require(activeDispatch == 0, @"nested client dispatch");
    activeDispatch = ++dispatchSequence;
    NSUInteger requestStart = serverRows().count;
    CGEventRef backing = CGEventCreateKeyboardEvent(keyboardSource, code, true);
    require(backing != NULL, @"cannot create owned Quartz key");
    @try {
        CGEventSetFlags(backing, 0);
        NSEvent *event = [NSEvent eventWithCGEvent:backing];
        require(event && event.type == NSEventTypeKeyDown && event.CGEvent, @"invalid backed keyDown");
        emit(@{@"event":@"key_enter", @"identity":client.identity, @"receiver":pointer(client), @"keycode":@(code),
            @"posted":@NO, @"source_state_id":@(CGEventGetIntegerValueField(backing, kCGEventSourceStateID))});
        [client keyDown:event];
        emit(@{@"event":@"key_return", @"identity":client.identity, @"keycode":@(code)});
    } @finally {CFRelease(backing); activeDispatch = 0;}
    __block NSDictionary *request = nil;
    waitFor(^BOOL {request = findServer(@"input_received", 0, requestStart); return request != nil;}, @"key did not reach IMK server");
    require([request[@"keycode"] unsignedIntegerValue] == code, @"unexpected IMK request");
    NSUInteger number = [request[@"request"] unsignedIntegerValue];
    require(number > 0, @"missing IMK request number");
    waitFor(^BOOL {return findServer(@"input_handler_return", number, requestStart) != nil;}, @"missing IMK handler return marker");
    emit(@{@"event":@"request_matched_after_key_return", @"request":@(number), @"keycode":@(code), @"identity":client.identity});
    return number;
}
static void control(NSString *operation, NSUInteger request) {
    require(activeDispatch == 0, @"control before native dispatch return");
    NSDictionary *message = @{@"seq":@(++controlSequence), @"operation":operation, @"request":@(request)};
    NSData *data = [NSJSONSerialization dataWithJSONObject:message options:NSJSONWritingSortedKeys error:nil];
    require([data writeToFile:[controlDirectory stringByAppendingPathComponent:@"control.json"] options:NSDataWritingAtomic error:nil],
        @"cannot write owned control");
    emit(@{@"event":@"control_sent", @"control":message});
    waitFor(^BOOL {
        for (NSDictionary *row in serverRows()) if ([row[@"event"] isEqual:@"control_applied"] && [row[@"control"] isEqual:message]) return YES;
        return NO;
    }, @"control effect not acknowledged");
    emit(@{@"event":@"control_effect_observed", @"control":message});
}
static NSDictionary *hold(AsyncClient *client) {
    phase = HoldingA;
    NSUInteger start = callbacks.count;
    NSUInteger initial = key(client, 0);
    NSString *initialText = [NSString stringWithFormat:@"active-%lu", (unsigned long)initial];
    waitFor(^BOOL {return callbackFound(start, client, @"preedit", initialText);}, @"initial native preedit absent");
    require([client.marked isEqual:initialText], @"initial composition not visible");
    NSUInteger request = key(client, 1);
    NSDictionary *held = findServer(@"response_held", request, 0);
    require(held && [held[@"preedit"] length] && [held[@"commit"] length], @"native response not held");
    require(!anyCallbackFound(start, @"preedit", held[@"preedit"]) && !anyCallbackFound(start, @"commit", held[@"commit"]),
        @"held tail was already delivered");
    emit(@{@"event":@"hold_qualified_after_dispatch_return", @"request":@(request), @"client":held[@"client"]});
    return held;
}
static NSDictionary *fresh(AsyncClient *client, CGKeyCode code) {
    NSUInteger start = callbacks.count;
    NSUInteger request = key(client, code);
    NSString *text = [NSString stringWithFormat:@"fresh-%lu", (unsigned long)request];
    waitFor(^BOOL {return callbackFound(start, client, @"preedit", text) && callbackFound(start, client, @"commit", text);},
        @"fresh native composition missing");
    return @{@"request":@(request), @"text":text};
}
static BOOL runCase(NSString *name, NSView *container, AsyncClient **previous) {
    caseName = name; phase = Initial;
    AsyncClient *a = [[AsyncClient alloc] initWithIdentity:[name stringByAppendingString:@"-A"] container:container];
    AsyncClient *b = [[AsyncClient alloc] initWithIdentity:[name stringByAppendingString:@"-B"] container:container];
    AsyncClient *s = [[AsyncClient alloc] initWithIdentity:[name stringByAppendingString:@"-sibling"] container:container];
    require(a.inputContext != b.inputContext && a.inputContext != s.inputContext && b.inputContext != s.inputContext,
        @"native views share contexts");
    focus(a, *previous);
    NSDictionary *held = hold(a);
    NSUInteger request = [held[@"request"] unsignedIntegerValue];
    NSString *marked = held[@"preedit"], *commit = held[@"commit"];
    NSUInteger start = callbacks.count;
    [challenges addObject:@{@"start":@(start), @"original_receiver":pointer(a), @"preedit":marked, @"commit":commit}];
    BOOL qualification = [name isEqual:@"qualify"];
    NSUInteger stateStart = states.count;
    if (!qualification) {
        phase = EmptyB; focus(b, a);
        stateStart = states.count - 1;
        key(b, 3);
        require(value(b).length == 0, @"B changed before old release");
        emit(@{@"event":@"new_B_empty_after_native_session_probe", @"value":value(b)});
    }
    phase = Releasing;
    NSString *duringFresh = @"";
    NSUInteger duringRequest = 0;
    if ([name isEqual:@"during_B_handler"]) {
        control(@"arm", request);
        NSDictionary *response = fresh(b, 14);
        duringFresh = response[@"text"]; duringRequest = [response[@"request"] unsignedIntegerValue];
    }
    else control(@"release", request);
    waitFor(^BOOL {return findServer(@"old_commit_send_return", request, 0) != nil;}, @"old response send did not return");
    waitFor(^BOOL {return anyCallbackFound(start, @"preedit", marked) && anyCallbackFound(start, @"commit", commit);},
        @"held response absent; silence is not rejection");
    observe(container.window, @"challenge_return");
    NSDictionary *send = findServer(@"old_response_send_begin", request, 0);
    require([send[@"input_depth"] unsignedIntegerValue] == (duringRequest ? 1 : 0), @"old response sent in unexpected handler scope");
    if (duringRequest) {
        NSDictionary *enter = findServer(@"input_received", duringRequest, 0);
        NSDictionary *returned = findServer(@"input_handler_return", duringRequest, 0);
        require([enter[@"seq"] unsignedIntegerValue] < [send[@"seq"] unsignedIntegerValue] &&
            [send[@"seq"] unsignedIntegerValue] < [returned[@"seq"] unsignedIntegerValue], @"old send not inside B handler");
    }
    if (qualification) {
        require(callbackFound(start, a, @"preedit", marked) && callbackFound(start, a, @"commit", commit) && [a.committed isEqual:commit],
            @"deferred A response routed elsewhere");
        for (NSUInteger i = start; i < callbacks.count; i++) {
            NSDictionary *event = callbacks[i];
            if ([event[@"text"] isEqual:marked] || [event[@"text"] isEqual:commit])
                require([event[@"dispatch"] unsignedIntegerValue] == 0, @"qualification tail arrived inside a native dispatch");
        }
        phase = Complete; *previous = a;
        emit(@{@"event":@"qualified", @"request":@(request), @"scope":@"DEFERRED_IME_RESPONSE_AFTER_ORIGINAL_KEY_RETURN"});
        return NO;
    }
    BOOL refuted = staleAt(start, b, marked, commit) || !ownerPreserved(stateStart, b);
    BOOL originalOwner = callbackFound(start, a, @"preedit", marked) && callbackFound(start, a, @"commit", commit);
    require(refuted || originalOwner, @"old response reached an unexpected receiver");
    refuted |= ![value(b) isEqual:duringFresh];
    phase = FreshB; NSString *expected = [duringFresh stringByAppendingString:fresh(b, 14)[@"text"]];
    refuted |= ![value(b) isEqual:expected] || !ownerPreserved(stateStart, b);
    NSString *beforeSibling = [value(b) copy];
    phase = Sibling; focus(s, b);
    NSString *expectedSibling = fresh(s, 34)[@"text"];
    require([value(s) isEqual:expectedSibling] && [value(b) isEqual:beforeSibling], @"sibling composition or B preservation failed");
    refuted |= staleAt(start, b, marked, commit);
    phase = Complete; *previous = s;
    emit(@{@"event":@"case_complete", @"request":@(request), @"refuted":@(refuted), @"old_owner_observed":@(originalOwner),
        @"B_value":value(b), @"sibling_value":value(s), @"scope":@"NATIVE_RECEIVER_EXPERIMENT_NOT_PRODUCT_ACCEPTANCE"});
    return refuted;
}

int main(int argc, const char *argv[]) {
    if (argc != 4) return 64;
#if !defined(__arm64__)
    fputs("ARM64-only IME client\n", stderr); return 77;
#endif
    if (!getenv("GITHUB_ACTIONS") || strcmp(getenv("GITHUB_ACTIONS"), "true") ||
        !getenv("RUNNER_OS") || strcmp(getenv("RUNNER_OS"), "macOS") ||
        !getenv("RUNNER_ENVIRONMENT") || strcmp(getenv("RUNNER_ENVIRONMENT"), "github-hosted")) {
        fputs("Hosted-CI-only IME client; local execution disabled\n", stderr); return 77;
    }
    @autoreleasepool {
        controlDirectory = [NSString stringWithUTF8String:argv[1]];
        serverLog = [NSString stringWithUTF8String:argv[2]];
        expectedSource = [NSString stringWithUTF8String:argv[3]];
        callbacks = [NSMutableArray new]; states = [NSMutableArray new];
        challenges = [NSMutableArray new];
        NSWindow *window = nil; int result = 2; BOOL ranAllCases = NO;
        @try {
            require([NSBundle.mainBundle.bundleIdentifier hasPrefix:@"com.geekjapan.soul2d.ci.client."], @"owned client app metadata required");
            expectedServerBundle = [NSBundle.mainBundle.bundleIdentifier stringByReplacingOccurrencesOfString:@".ci.client." withString:@".ci.ime."];
            require(expectedSource.length > 0, @"owned input source identifier required");
            [NSApplication sharedApplication];
            require([NSApp setActivationPolicy:NSApplicationActivationPolicyRegular], @"cannot activate owned client");
            [NSApp finishLaunching];
            keyboardSource = CGEventSourceCreate(kCGEventSourceStatePrivate);
            require(keyboardSource != NULL, @"cannot create private keyboard source");
            window = [[NSWindow alloc] initWithContentRect:NSMakeRect(100, 100, 640, 360)
                styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable backing:NSBackingStoreBuffered defer:NO];
            window.releasedWhenClosed = NO; window.title = @"Soul2D CI asynchronous IME";
            [window makeKeyAndOrderFront:nil]; [NSApp activateIgnoringOtherApps:YES];
            waitFor(^BOOL {return window.isKeyWindow;}, @"owned window did not become key");
            AsyncClient *previous = nil;
            runCase(@"qualify", window.contentView, &previous);
            BOOL refuted = runCase(@"after_B_focus", window.contentView, &previous);
            refuted |= runCase(@"during_B_handler", window.contentView, &previous);
            result = refuted ? 3 : 0;
            ranAllCases = YES;
        } @catch (NSException *error) {
            emit(@{@"event":@"incomplete", @"name":error.name, @"reason":error.reason ?: @"unknown"});
        } @finally {
            [window orderOut:nil]; [window close];
            if (keyboardSource) {CFRelease(keyboardSource); keyboardSource = NULL;}
            if (result == 0 && cumulativeRefutation()) result = 3;
            emit(@{@"event":@"client_cleanup", @"window_closed":@YES, @"keyboard_source_released":@YES,
                @"status":@(result), @"last_control_seq":@(controlSequence)});
        }
        if (ranAllCases) emit(@{@"event":@"experiment_complete", @"refuted":@(result == 3), @"last_control_seq":@(controlSequence),
            @"physical_IME":@"NOT_RUN", @"product_acceptance":@"NOT_ESTABLISHED"});
        return result;
    }
}
