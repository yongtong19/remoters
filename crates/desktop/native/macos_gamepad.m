#import <Foundation/Foundation.h>
#import <GameController/GameController.h>
#include <math.h>
#include <pthread.h>
#include <stdint.h>
#include <string.h>

typedef struct {
    uint8_t slot;
    uint8_t connected;
    uint16_t buttons;
    uint8_t left_trigger;
    uint8_t right_trigger;
    int16_t left_x;
    int16_t left_y;
    int16_t right_x;
    int16_t right_y;
} RemotersGamepadState;

enum {
    REMOTERS_GP_DPAD_UP = 0x0001,
    REMOTERS_GP_DPAD_DOWN = 0x0002,
    REMOTERS_GP_DPAD_LEFT = 0x0004,
    REMOTERS_GP_DPAD_RIGHT = 0x0008,
    REMOTERS_GP_START = 0x0010,
    REMOTERS_GP_BACK = 0x0020,
    REMOTERS_GP_LEFT_THUMB = 0x0040,
    REMOTERS_GP_RIGHT_THUMB = 0x0080,
    REMOTERS_GP_LEFT_SHOULDER = 0x0100,
    REMOTERS_GP_RIGHT_SHOULDER = 0x0200,
    REMOTERS_GP_GUIDE = 0x0400,
    REMOTERS_GP_A = 0x1000,
    REMOTERS_GP_B = 0x2000,
    REMOTERS_GP_X = 0x4000,
    REMOTERS_GP_Y = 0x8000,
};

static pthread_mutex_t g_gamepad_lock = PTHREAD_MUTEX_INITIALIZER;
static GCController *g_slots[4] = {nil, nil, nil, nil};
static bool g_initialized = false;

static inline uint8_t float_to_u8(float value) {
    if (!(value == value) || value <= 0.0f) {
        return 0;
    }
    if (value >= 1.0f) {
        return 255;
    }
    long rounded = lrintf(value * 255.0f);
    if (rounded < 0) {
        return 0;
    }
    if (rounded > 255) {
        return 255;
    }
    return (uint8_t)rounded;
}

static inline int16_t float_to_i16(float value) {
    if (!(value == value)) {
        return 0;
    }
    if (value >= 1.0f) {
        return 32767;
    }
    if (value <= -1.0f) {
        return -32768;
    }
    long rounded = lrintf(value * 32767.0f);
    if (rounded > 32767) {
        return 32767;
    }
    if (rounded < -32768) {
        return -32768;
    }
    return (int16_t)rounded;
}

void remoters_macos_gamepad_init(void) {
    pthread_mutex_lock(&g_gamepad_lock);
    if (!g_initialized) {
        g_initialized = true;
        if (@available(macOS 11.3, *)) {
            GCController.shouldMonitorBackgroundEvents = YES;
        }
    }
    pthread_mutex_unlock(&g_gamepad_lock);
}

void remoters_macos_gamepad_poll(RemotersGamepadState out_states[4]) {
    if (!out_states) {
        return;
    }
    remoters_macos_gamepad_init();

    @autoreleasepool {
        NSArray<GCController *> *controllers = [GCController controllers];
        pthread_mutex_lock(&g_gamepad_lock);

        for (int i = 0; i < 4; ++i) {
            GCController *existing = g_slots[i];
            if (!existing) {
                continue;
            }
            bool still_present = false;
            for (GCController *c in controllers) {
                if (c == existing && c.extendedGamepad != nil) {
                    still_present = true;
                    break;
                }
            }
            if (!still_present) {
                g_slots[i] = nil;
            }
        }

        for (GCController *c in controllers) {
            if (c.extendedGamepad == nil) {
                continue;
            }
            bool assigned = false;
            for (int i = 0; i < 4; ++i) {
                if (g_slots[i] == c) {
                    assigned = true;
                    break;
                }
            }
            if (assigned) {
                continue;
            }
            for (int i = 0; i < 4; ++i) {
                if (g_slots[i] == nil) {
                    g_slots[i] = c;
                    break;
                }
            }
        }

        for (int i = 0; i < 4; ++i) {
            memset(&out_states[i], 0, sizeof(RemotersGamepadState));
            out_states[i].slot = (uint8_t)i;
            GCController *c = g_slots[i];
            if (!c) {
                continue;
            }
            GCExtendedGamepad *gp = c.extendedGamepad;
            if (!gp) {
                continue;
            }

            uint16_t buttons = 0;
            if (gp.dpad.up.isPressed || gp.dpad.yAxis.value > 0.5f) {
                buttons |= REMOTERS_GP_DPAD_UP;
            }
            if (gp.dpad.down.isPressed || gp.dpad.yAxis.value < -0.5f) {
                buttons |= REMOTERS_GP_DPAD_DOWN;
            }
            if (gp.dpad.left.isPressed || gp.dpad.xAxis.value < -0.5f) {
                buttons |= REMOTERS_GP_DPAD_LEFT;
            }
            if (gp.dpad.right.isPressed || gp.dpad.xAxis.value > 0.5f) {
                buttons |= REMOTERS_GP_DPAD_RIGHT;
            }
            if (gp.buttonMenu.isPressed) {
                buttons |= REMOTERS_GP_START;
            }
            if (gp.buttonOptions && gp.buttonOptions.isPressed) {
                buttons |= REMOTERS_GP_BACK;
            }
            if (gp.leftThumbstickButton && gp.leftThumbstickButton.isPressed) {
                buttons |= REMOTERS_GP_LEFT_THUMB;
            }
            if (gp.rightThumbstickButton && gp.rightThumbstickButton.isPressed) {
                buttons |= REMOTERS_GP_RIGHT_THUMB;
            }
            if (gp.leftShoulder.isPressed) {
                buttons |= REMOTERS_GP_LEFT_SHOULDER;
            }
            if (gp.rightShoulder.isPressed) {
                buttons |= REMOTERS_GP_RIGHT_SHOULDER;
            }
            if (@available(macOS 11.0, *)) {
                if (gp.buttonHome && gp.buttonHome.isPressed) {
                    buttons |= REMOTERS_GP_GUIDE;
                }
            }
            if (gp.buttonA.isPressed) {
                buttons |= REMOTERS_GP_A;
            }
            if (gp.buttonB.isPressed) {
                buttons |= REMOTERS_GP_B;
            }
            if (gp.buttonX.isPressed) {
                buttons |= REMOTERS_GP_X;
            }
            if (gp.buttonY.isPressed) {
                buttons |= REMOTERS_GP_Y;
            }

            out_states[i].connected = 1;
            out_states[i].buttons = buttons;
            out_states[i].left_trigger = float_to_u8(gp.leftTrigger.value);
            out_states[i].right_trigger = float_to_u8(gp.rightTrigger.value);
            out_states[i].left_x = float_to_i16(gp.leftThumbstick.xAxis.value);
            out_states[i].left_y = float_to_i16(gp.leftThumbstick.yAxis.value);
            out_states[i].right_x = float_to_i16(gp.rightThumbstick.xAxis.value);
            out_states[i].right_y = float_to_i16(gp.rightThumbstick.yAxis.value);
        }

        pthread_mutex_unlock(&g_gamepad_lock);
    }
}
