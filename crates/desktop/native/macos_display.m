#import <AppKit/AppKit.h>
#import <CoreGraphics/CoreGraphics.h>
#import <Foundation/Foundation.h>
#include <math.h>
#include <stdbool.h>
#include <unistd.h>

// CoreGraphics has no public API for creating an independent display. Resolve
// the selectors at runtime so sharing an existing display still works if the
// system removes this SPI.
@protocol RemotersDisplayDescriptor
- (void)setName:(NSString *)name;
- (void)setQueue:(dispatch_queue_t)queue;
- (void)setMaxPixelsWide:(unsigned)width;
- (void)setMaxPixelsHigh:(unsigned)height;
- (void)setSizeInMillimeters:(CGSize)size;
- (void)setVendorID:(unsigned)vendor;
- (void)setProductID:(unsigned)product;
- (void)setSerialNum:(unsigned)serial;
- (void)setSerialNumber:(unsigned)serial;
- (void)setWhitePoint:(CGPoint)point;
- (void)setRedPrimary:(CGPoint)point;
- (void)setGreenPrimary:(CGPoint)point;
- (void)setBluePrimary:(CGPoint)point;
@end

@protocol RemotersDisplayMode
- (instancetype)initWithWidth:(unsigned)width height:(unsigned)height refreshRate:(double)hz;
@end

@protocol RemotersDisplaySettings
- (void)setHiDPI:(unsigned)hidpi;
- (void)setModes:(NSArray *)modes;
@end

@protocol RemotersDisplay
- (instancetype)initWithDescriptor:(id)descriptor;
- (BOOL)applySettings:(id)settings;
- (CGDirectDisplayID)displayID;
@end

static BOOL activate_mode(CGDirectDisplayID display, uint32_t width, uint32_t height, uint32_t fps) {
    for (int attempt = 0; attempt < 30; ++attempt) {
        CFArrayRef modes = CGDisplayCopyAllDisplayModes(display, NULL);
        if (modes) {
            CGDisplayModeRef exact_match = NULL;
            CGDisplayModeRef fallback_match = NULL;
            const CFIndex count = CFArrayGetCount(modes);
            for (CFIndex index = 0; index < count; ++index) {
                CGDisplayModeRef mode = (CGDisplayModeRef)CFArrayGetValueAtIndex(modes, index);
                if (CGDisplayModeGetPixelWidth(mode) != width ||
                    CGDisplayModeGetPixelHeight(mode) != height) {
                    continue;
                }
                const double rate = CGDisplayModeGetRefreshRate(mode);
                if (rate >= (double)fps - 0.1) {
                    exact_match = mode;
                    break;
                }
                if (!fallback_match && rate <= 0.0) {
                    fallback_match = mode;
                }
            }
            CGDisplayModeRef chosen = exact_match ? exact_match : fallback_match;
            BOOL found = chosen && CGDisplaySetDisplayMode(display, chosen, NULL) == kCGErrorSuccess;
            CFRelease(modes);
            if (found && CGDisplayIsActive(display) &&
                CGDisplayPixelsWide(display) == width && CGDisplayPixelsHigh(display) == height) {
                return YES;
            }
        }
        usleep(50000);
    }
    return NO;
}

bool remoters_virtual_display_available(void) {
    @autoreleasepool {
        return NSClassFromString(@"CGVirtualDisplayDescriptor") != Nil &&
               NSClassFromString(@"CGVirtualDisplay") != Nil &&
               NSClassFromString(@"CGVirtualDisplayMode") != Nil &&
               NSClassFromString(@"CGVirtualDisplaySettings") != Nil;
    }
}

void *remoters_virtual_display_create(
    uint32_t width, uint32_t height, uint32_t fps, uint32_t *display_id) {
    @autoreleasepool {
        @try {
            Class descriptor_class = NSClassFromString(@"CGVirtualDisplayDescriptor");
            Class display_class = NSClassFromString(@"CGVirtualDisplay");
            Class mode_class = NSClassFromString(@"CGVirtualDisplayMode");
            Class settings_class = NSClassFromString(@"CGVirtualDisplaySettings");
            if (!descriptor_class || !display_class || !mode_class || !settings_class) return NULL;

            NSObject<RemotersDisplayDescriptor> *descriptor = [[descriptor_class alloc] init];
            [descriptor setName:@"Remoters Virtual Display"];
            [descriptor setQueue:dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0)];
            [descriptor setMaxPixelsWide:width];
            [descriptor setMaxPixelsHigh:height];
            [descriptor setSizeInMillimeters:CGSizeMake(width / 5.0, height / 5.0)];
            const uint32_t serial = arc4random_uniform(UINT32_MAX - 1) + 1;
            [descriptor setVendorID:0x5253];
            [descriptor setProductID:1];
            [descriptor setSerialNum:serial];
            if ([descriptor respondsToSelector:@selector(setSerialNumber:)]) {
                [descriptor setSerialNumber:serial];
            }
            [descriptor setWhitePoint:CGPointMake(0.3127, 0.3290)];
            [descriptor setRedPrimary:CGPointMake(0.6400, 0.3300)];
            [descriptor setGreenPrimary:CGPointMake(0.3000, 0.6000)];
            [descriptor setBluePrimary:CGPointMake(0.1500, 0.0600)];

            id<RemotersDisplay> display =
                [(id<RemotersDisplay>)[display_class alloc] initWithDescriptor:descriptor];
            id<RemotersDisplayMode> mode =
                [(id<RemotersDisplayMode>)[mode_class alloc] initWithWidth:width height:height refreshRate:fps];
            NSObject<RemotersDisplaySettings> *settings = [[settings_class alloc] init];
            if (!display || !mode || !settings) return NULL;
            [settings setHiDPI:0];
            [settings setModes:@[mode]];
            if (![display applySettings:settings]) return NULL;
            const CGDirectDisplayID id = [display displayID];
            if (!id || !activate_mode(id, width, height, fps)) return NULL;
            *display_id = id;
            return (__bridge_retained void *)display;
        } @catch (NSException *exception) {
            (void)exception;
            return NULL;
        }
    }
}

void remoters_virtual_display_destroy(void *display) {
    @autoreleasepool {
        id retained_display = (__bridge_transfer id)display;
        (void)retained_display;
    }
}

bool remoters_macos_fullscreen_pixels(uint32_t *display_id, uint32_t *width, uint32_t *height) {
    if (!width || !height || ![NSThread isMainThread]) return false;
    @autoreleasepool {
        NSScreen *screen = [NSScreen mainScreen];
        if (!screen) screen = NSScreen.screens.firstObject;
        if (!screen) return false;
        CGFloat scale = screen.backingScaleFactor;
        if (scale < 1.0) scale = 1.0;
        NSRect frame = screen.frame;
        NSEdgeInsets insets = {0, 0, 0, 0};
        if (@available(macOS 12.0, *)) {
            insets = screen.safeAreaInsets;
        }
        const double usable_w = (frame.size.width - insets.left - insets.right) * scale;
        const double usable_h = (frame.size.height - insets.top - insets.bottom) * scale;
        if (usable_w < 2.0 || usable_h < 2.0) return false;
        if (display_id) {
            NSNumber *num = screen.deviceDescription[@"NSScreenNumber"];
            *display_id = num ? num.unsignedIntValue : 0;
        }
        *width = (uint32_t)llround(usable_w);
        *height = (uint32_t)llround(usable_h);
        return true;
    }
}
