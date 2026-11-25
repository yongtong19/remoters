#import <CoreGraphics/CoreGraphics.h>
#import <Foundation/Foundation.h>
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
            BOOL found = NO;
            for (CFIndex index = 0; index < CFArrayGetCount(modes); ++index) {
                CGDisplayModeRef mode = (CGDisplayModeRef)CFArrayGetValueAtIndex(modes, index);
                if (CGDisplayModeGetPixelWidth(mode) != width ||
                    CGDisplayModeGetPixelHeight(mode) != height ||
                    CGDisplayModeGetRefreshRate(mode) < fps - 0.1) continue;
                found = CGDisplaySetDisplayMode(display, mode, NULL) == kCGErrorSuccess;
                if (found) break;
            }
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
            [descriptor setSizeInMillimeters:CGSizeMake(width / 6.0, height / 6.0)];
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
