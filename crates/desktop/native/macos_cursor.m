#import <AppKit/AppKit.h>
#import <CoreGraphics/CoreGraphics.h>
#import <Foundation/Foundation.h>
#include <math.h>
#include <pthread.h>
#include <stdint.h>

enum {
    REMOTERS_CURSOR_ARROW = 0,
    REMOTERS_CURSOR_IBEAM = 1,
    REMOTERS_CURSOR_CROSSHAIR = 2,
    REMOTERS_CURSOR_CLOSED_HAND = 3,
    REMOTERS_CURSOR_OPEN_HAND = 4,
    REMOTERS_CURSOR_POINTING_HAND = 5,
    REMOTERS_CURSOR_RESIZE_LEFT = 6,
    REMOTERS_CURSOR_RESIZE_RIGHT = 7,
    REMOTERS_CURSOR_RESIZE_LEFT_RIGHT = 8,
    REMOTERS_CURSOR_RESIZE_UP = 9,
    REMOTERS_CURSOR_RESIZE_DOWN = 10,
    REMOTERS_CURSOR_RESIZE_UP_DOWN = 11,
    REMOTERS_CURSOR_RESIZE_NWSE = 12,
    REMOTERS_CURSOR_RESIZE_NESW = 13,
    REMOTERS_CURSOR_IBEAM_VERTICAL = 14,
    REMOTERS_CURSOR_OPERATION_NOT_ALLOWED = 15,
    REMOTERS_CURSOR_DRAG_LINK = 16,
    REMOTERS_CURSOR_DRAG_COPY = 17,
    REMOTERS_CURSOR_CONTEXTUAL_MENU = 18,
    REMOTERS_CURSOR_HIDDEN = 19,
};

typedef struct {
    uint64_t hash;
    size_t len;
    uint8_t shape;
} CursorSignature;

static uint64_t fnv1a_hash(const uint8_t *bytes, size_t len) {
    uint64_t h = 14695981039346656037ULL;
    for (size_t i = 0; i < len; ++i) {
        h ^= bytes[i];
        h *= 1099511628211ULL;
    }
    return h;
}

static bool cursor_signature(NSCursor *cursor, uint64_t *out_hash, size_t *out_len) {
    if (!cursor) return false;
    NSImage *image = cursor.image;
    if (!image) return false;

    NSRect proposed = NSMakeRect(0, 0, image.size.width, image.size.height);
    CGImageRef cg = [image CGImageForProposedRect:&proposed context:nil hints:nil];
    if (cg) {
        CFDataRef data = CGDataProviderCopyData(CGImageGetDataProvider(cg));
        if (data) {
            const uint8_t *bytes = CFDataGetBytePtr(data);
            CFIndex length = CFDataGetLength(data);
            uint64_t h = fnv1a_hash(bytes, (size_t)length);
            h ^= ((uint64_t)llround(cursor.hotSpot.x * 10.0)) << 32;
            h ^= (uint64_t)llround(cursor.hotSpot.y * 10.0);
            *out_hash = h;
            *out_len = (size_t)length;
            CFRelease(data);
            return true;
        }
    }

    NSData *tiff = image.TIFFRepresentation;
    if (!tiff) return false;
    uint64_t h = fnv1a_hash((const uint8_t *)tiff.bytes, (size_t)tiff.length);
    h ^= ((uint64_t)llround(cursor.hotSpot.x * 10.0)) << 32;
    h ^= (uint64_t)llround(cursor.hotSpot.y * 10.0);
    *out_hash = h;
    *out_len = (size_t)tiff.length;
    return true;
}

static pthread_mutex_t g_cursor_lock = PTHREAD_MUTEX_INITIALIZER;
static CursorSignature g_table[24];
static size_t g_table_len = 0;

static void append_cursor(SEL selector, uint8_t shape) {
    if (g_table_len >= sizeof(g_table) / sizeof(g_table[0])) return;
    if (![NSCursor respondsToSelector:selector]) return;
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Warc-performSelector-leaks"
    NSCursor *cursor = [NSCursor performSelector:selector];
#pragma clang diagnostic pop
    uint64_t hash = 0;
    size_t len = 0;
    if (cursor_signature(cursor, &hash, &len)) {
        g_table[g_table_len++] = (CursorSignature){
            .hash = hash,
            .len = len,
            .shape = shape,
        };
    }
}

static void ensure_cursor_table_locked(void) {
    if (g_table_len > 0) return;

#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
    append_cursor(@selector(arrowCursor), REMOTERS_CURSOR_ARROW);
    append_cursor(@selector(IBeamCursor), REMOTERS_CURSOR_IBEAM);
    append_cursor(@selector(pointingHandCursor), REMOTERS_CURSOR_POINTING_HAND);
    append_cursor(@selector(crosshairCursor), REMOTERS_CURSOR_CROSSHAIR);
    append_cursor(@selector(openHandCursor), REMOTERS_CURSOR_OPEN_HAND);
    append_cursor(@selector(closedHandCursor), REMOTERS_CURSOR_CLOSED_HAND);
    append_cursor(@selector(resizeLeftRightCursor), REMOTERS_CURSOR_RESIZE_LEFT_RIGHT);
    append_cursor(@selector(resizeUpDownCursor), REMOTERS_CURSOR_RESIZE_UP_DOWN);
    append_cursor(@selector(resizeLeftCursor), REMOTERS_CURSOR_RESIZE_LEFT);
    append_cursor(@selector(resizeRightCursor), REMOTERS_CURSOR_RESIZE_RIGHT);
    append_cursor(@selector(resizeUpCursor), REMOTERS_CURSOR_RESIZE_UP);
    append_cursor(@selector(resizeDownCursor), REMOTERS_CURSOR_RESIZE_DOWN);
    append_cursor(@selector(operationNotAllowedCursor), REMOTERS_CURSOR_OPERATION_NOT_ALLOWED);
    append_cursor(@selector(dragCopyCursor), REMOTERS_CURSOR_DRAG_COPY);
    append_cursor(@selector(dragLinkCursor), REMOTERS_CURSOR_DRAG_LINK);
    append_cursor(@selector(contextualMenuCursor), REMOTERS_CURSOR_CONTEXTUAL_MENU);
    append_cursor(@selector(IBeamCursorForVerticalLayout), REMOTERS_CURSOR_IBEAM_VERTICAL);
    append_cursor(
        NSSelectorFromString(@"_windowResizeNorthWestSouthEastCursor"),
        REMOTERS_CURSOR_RESIZE_NWSE
    );
    append_cursor(
        NSSelectorFromString(@"_windowResizeNorthEastSouthWestCursor"),
        REMOTERS_CURSOR_RESIZE_NESW
    );
#pragma clang diagnostic pop
}

uint8_t remoters_macos_cursor_shape(void) {
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
    if (!CGCursorIsVisible()) {
        return REMOTERS_CURSOR_HIDDEN;
    }
#pragma clang diagnostic pop

    @autoreleasepool {
        @try {
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
            NSCursor *current = [NSCursor currentSystemCursor];
#pragma clang diagnostic pop
            if (!current) {
                return REMOTERS_CURSOR_ARROW;
            }
            uint64_t hash = 0;
            size_t len = 0;
            if (!cursor_signature(current, &hash, &len)) {
                return REMOTERS_CURSOR_ARROW;
            }

            pthread_mutex_lock(&g_cursor_lock);
            ensure_cursor_table_locked();
            uint8_t matched = REMOTERS_CURSOR_ARROW;
            for (size_t i = 0; i < g_table_len; ++i) {
                if (g_table[i].len == len && g_table[i].hash == hash) {
                    matched = g_table[i].shape;
                    break;
                }
            }
            pthread_mutex_unlock(&g_cursor_lock);
            return matched;
        } @catch (NSException *exception) {
            (void)exception;
            return REMOTERS_CURSOR_ARROW;
        }
    }
}
