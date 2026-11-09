#import <Foundation/Foundation.h>
#import <Carbon/Carbon.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void require(BOOL condition, NSString *reason) {
    if (!condition) @throw [NSException exceptionWithName:@"LifecycleError" reason:reason userInfo:nil];
}
static NSString *textProperty(TISInputSourceRef source, CFStringRef key) {
    CFTypeRef value = TISGetInputSourceProperty(source, key);
    require(value && CFGetTypeID(value) == CFStringGetTypeID(), @"missing input-source string property");
    return [(__bridge NSString *)value copy];
}
static BOOL flag(TISInputSourceRef source, CFStringRef key) {
    CFTypeRef value = TISGetInputSourceProperty(source, key);
    require(value && CFGetTypeID(value) == CFBooleanGetTypeID(), @"missing input-source boolean property");
    return CFBooleanGetValue((CFBooleanRef)value);
}
static NSDictionary *describe(TISInputSourceRef source) {
    require(source != NULL, @"missing input source");
    return @{@"id":textProperty(source, kTISPropertyInputSourceID),
        @"enabled":@(flag(source, kTISPropertyInputSourceIsEnabled)),
        @"selected":@(flag(source, kTISPropertyInputSourceIsSelected)),
        @"selectable":@(flag(source, kTISPropertyInputSourceIsSelectCapable))};
}
static NSDictionary *current(void) {
    TISInputSourceRef source = TISCopyCurrentKeyboardInputSource();
    @try {return describe(source);}
    @finally {if (source) CFRelease(source);}
}
static NSArray *sources(NSString *bundle, NSString *identifier) {
    NSMutableDictionary *filter = [NSMutableDictionary new];
    if (bundle) filter[(__bridge NSString *)kTISPropertyBundleID] = bundle;
    if (identifier) filter[(__bridge NSString *)kTISPropertyInputSourceID] = identifier;
    NSArray *result = CFBridgingRelease(TISCreateInputSourceList((__bridge CFDictionaryRef)filter, true));
    require(result != nil, @"cannot enumerate input sources");
    return result;
}
static BOOL digits(NSString *value) {
    return value.length > 0 && value.length <= 20 &&
        [value rangeOfCharacterFromSet:[[NSCharacterSet characterSetWithCharactersInString:@"0123456789"] invertedSet]].location == NSNotFound;
}
static void emit(NSDictionary *row) {
    NSData *data = [NSJSONSerialization dataWithJSONObject:row options:NSJSONWritingSortedKeys error:nil];
    require(data && fwrite(data.bytes, 1, data.length, stdout) == data.length && fputc('\n', stdout) != EOF, @"cannot write TIS receipt");
    fflush(stdout);
}
int main(int argc, const char *argv[]) {
    if (argc < 2 || argc > 4) return 64;
#if !defined(__arm64__)
    fputs("ARM64-only TIS helper\n", stderr); return 77;
#endif
    if (!getenv("GITHUB_ACTIONS") || strcmp(getenv("GITHUB_ACTIONS"), "true") ||
        !getenv("RUNNER_OS") || strcmp(getenv("RUNNER_OS"), "macOS") ||
        !getenv("RUNNER_ENVIRONMENT") || strcmp(getenv("RUNNER_ENVIRONMENT"), "github-hosted")) {
        fputs("Hosted-CI-only TIS helper; local configuration access disabled\n", stderr); return 77;
    }
    @autoreleasepool {
        @try {
            NSString *run = NSProcessInfo.processInfo.environment[@"GITHUB_RUN_ID"];
            NSString *attempt = NSProcessInfo.processInfo.environment[@"GITHUB_RUN_ATTEMPT"];
            require(digits(run) && digits(attempt), @"numeric run and attempt required");
            NSString *owned = [NSString stringWithFormat:@"com.geekjapan.soul2d.ci.ime.r%@.a%@", run, attempt];
            NSString *operation = [NSString stringWithUTF8String:argv[1]];
            OSStatus status = noErr;
            if ([operation isEqual:@"current"]) {
                require(argc == 2, @"current takes no arguments");
                emit(@{@"operation":operation, @"source":current()}); return 0;
            }
            require(argc >= 3, @"missing operation argument");
            NSString *argument = [NSString stringWithUTF8String:argv[2]];
            if ([operation isEqual:@"register"]) {
                require(argc == 3, @"register takes one bundle path");
                NSString *name = [NSString stringWithFormat:@"Soul2D-CI-IME-%@-%@.app", run, attempt];
                NSString *path = [[NSHomeDirectory() stringByAppendingPathComponent:@"Library/Input Methods"] stringByAppendingPathComponent:name];
                require([argument isEqual:path] && [argument isEqual:argument.stringByResolvingSymlinksInPath], @"unexpected or symlinked input-method path");
                NSBundle *bundle = [NSBundle bundleWithPath:path];
                require([bundle.bundleIdentifier isEqual:owned] && [bundle.infoDictionary[@"CFBundleExecutable"] isEqual:@"S2DCIAsyncIME"], @"unexpected owned bundle metadata");
                status = TISRegisterInputSource((__bridge CFURLRef)[NSURL fileURLWithPath:path]);
            } else if ([operation isEqual:@"list"]) {
                require(argc == 3 && [argument isEqual:owned], @"list accepts only the owned bundle");
                NSMutableArray *rows = [NSMutableArray new];
                for (id item in sources(owned, nil)) [rows addObject:describe((__bridge TISInputSourceRef)item)];
                emit(@{@"operation":operation, @"sources":rows}); return 0;
            } else {
                BOOL restore = [operation isEqual:@"restore"];
                require(restore || [operation isEqual:@"enable"] || [operation isEqual:@"select"] || [operation isEqual:@"disable"], @"unknown TIS operation");
                require(restore ? argc == 3 : (argc == 4 && [argument isEqual:owned]), @"unexpected TIS operation arguments");
                NSString *identifier = restore ? argument : [NSString stringWithUTF8String:argv[3]];
                require(identifier.length > 0, @"input-source identifier required");
                NSArray *matches = sources(restore ? nil : owned, identifier);
                require(matches.count == 1, @"input source is missing or ambiguous");
                TISInputSourceRef source = (__bridge TISInputSourceRef)matches[0];
                if ([operation isEqual:@"enable"]) {
                    if (!flag(source, kTISPropertyInputSourceIsEnabled)) status = TISEnableInputSource(source);
                } else if ([operation isEqual:@"disable"]) {
                    require(!flag(source, kTISPropertyInputSourceIsSelected), @"cannot disable selected source before restoration");
                    if (flag(source, kTISPropertyInputSourceIsEnabled)) status = TISDisableInputSource(source);
                } else {
                    require(flag(source, kTISPropertyInputSourceIsEnabled) && flag(source, kTISPropertyInputSourceIsSelectCapable), @"source is not enabled and selectable");
                    if (!flag(source, kTISPropertyInputSourceIsSelected)) status = TISSelectInputSource(source);
                }
            }
            NSMutableDictionary *receipt = [@{@"operation":operation, @"os_status":@(status)} mutableCopy];
            @try {receipt[@"current"] = current();}
            @catch (NSException *error) {receipt[@"observation_error"] = error.reason ?: @"unknown";}
            emit(receipt);
            return status == noErr && receipt[@"current"] ? 0 : 2;
        } @catch (NSException *error) {
            emit(@{@"event":@"tis_error", @"name":error.name, @"reason":error.reason ?: @"unknown", @"effect":@"UNKNOWN_NO_AUTOMATIC_RETRY"});
            return 2;
        }
    }
}
