#import <AppKit/AppKit.h>
#import <CoreGraphics/CoreGraphics.h>
#import <Foundation/Foundation.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

static void *create_privacy_windows_on_main(
    bool is_virtual_display,
    uint32_t captured_display_id,
    uint32_t *out_window_ids,
    size_t max_window_ids,
    size_t *out_count
) {
    @autoreleasepool {
        @try {
            NSArray<NSScreen *> *screens = [NSScreen screens];
            if (!screens || screens.count == 0) {
                if (out_count) *out_count = 0;
                return NULL;
            }

            NSMutableArray<NSWindow *> *windows = [NSMutableArray arrayWithCapacity:screens.count];
            size_t count = 0;

            for (NSScreen *screen in screens) {
                NSNumber *screen_num = screen.deviceDescription[@"NSScreenNumber"];
                uint32_t screen_id = screen_num ? screen_num.unsignedIntValue : 0;
                if (is_virtual_display && captured_display_id != 0 &&
                    screen_id == captured_display_id) {
                    continue;
                }

                NSRect frame = screen.frame;
                if (frame.size.width <= 0.0 || frame.size.height <= 0.0) {
                    continue;
                }

                NSWindow *window = [[NSWindow alloc]
                    initWithContentRect:frame
                              styleMask:NSWindowStyleMaskBorderless
                                backing:NSBackingStoreBuffered
                                  defer:NO];
                if (!window) {
                    continue;
                }

                [window setFrame:frame display:YES];
                [window setBackgroundColor:[NSColor blackColor]];
                [window setOpaque:YES];
                [window setHasShadow:NO];
                [window setLevel:NSScreenSaverWindowLevel];
                [window setSharingType:NSWindowSharingNone];
                [window setIgnoresMouseEvents:YES];
                [window setReleasedWhenClosed:NO];
                [window setCollectionBehavior:
                    NSWindowCollectionBehaviorCanJoinAllSpaces |
                    NSWindowCollectionBehaviorStationary |
                    NSWindowCollectionBehaviorFullScreenAuxiliary |
                    NSWindowCollectionBehaviorIgnoresCycle];
                [window orderFrontRegardless];

                [windows addObject:window];

                NSInteger window_num = [window windowNumber];
                if (window_num > 0 && out_window_ids && count < max_window_ids) {
                    out_window_ids[count++] = (uint32_t)window_num;
                }
            }

            if (out_count) {
                *out_count = count;
            }
            if (windows.count == 0) {
                return NULL;
            }
            return (__bridge_retained void *)windows;
        } @catch (NSException *exception) {
            (void)exception;
            if (out_count) *out_count = 0;
            return NULL;
        }
    }
}

void *remoters_macos_privacy_create(
    bool is_virtual_display,
    uint32_t captured_display_id,
    uint32_t *out_window_ids,
    size_t max_window_ids,
    size_t *out_count
) {
    if (out_count) {
        *out_count = 0;
    }
    if (NSApp == nil) {
        return NULL;
    }
    if ([NSThread isMainThread]) {
        return create_privacy_windows_on_main(
            is_virtual_display,
            captured_display_id,
            out_window_ids,
            max_window_ids,
            out_count
        );
    }
    if (![NSApp isRunning]) {
        return NULL;
    }
    __block void *handle = NULL;
    dispatch_sync(dispatch_get_main_queue(), ^{
        handle = create_privacy_windows_on_main(
            is_virtual_display,
            captured_display_id,
            out_window_ids,
            max_window_ids,
            out_count
        );
    });
    return handle;
}

void remoters_macos_privacy_destroy(void *handle) {
    if (!handle) {
        return;
    }
    NSArray<NSWindow *> *windows = (__bridge_transfer NSArray<NSWindow *> *)handle;
    void (^cleanup)(void) = ^{
        @autoreleasepool {
            @try {
                for (NSWindow *window in windows) {
                    [window orderOut:nil];
                    [window close];
                }
            } @catch (NSException *exception) {
                (void)exception;
            }
        }
    };
    if ([NSThread isMainThread]) {
        cleanup();
    } else {
        dispatch_async(dispatch_get_main_queue(), cleanup);
    }
}
