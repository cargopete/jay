// System audio capture on macOS via a CoreAudio process tap.
//
// `CATapDescription` is an Objective-C class, so this cannot be done from Rust
// with C FFI alone. The shim keeps the Objective-C confined to one file and
// hands Rust a small C surface.
//
// The route is: create a global tap, wrap it in a private aggregate device,
// attach an IOProc, and mix whatever arrives down to mono. Mixing here rather
// than in Rust avoids having to handle both interleaved and planar buffer
// layouts on both sides of the boundary.
//
// Requires macOS 14.4 or newer, and the user's consent. There is no way to do
// this without consent, and jay would not want one.

#import <AudioToolbox/AudioToolbox.h>
#import <CoreAudio/AudioHardwareTapping.h>
#import <CoreAudio/CATapDescription.h>
#import <CoreAudio/CoreAudio.h>
#import <Foundation/Foundation.h>

typedef void (*JayTapCallback)(void *ctx, const float *mono, uint32_t frames);

typedef struct JayTap {
    AudioObjectID tap;
    AudioObjectID aggregate;
    AudioDeviceIOProcID ioProc;
    JayTapCallback callback;
    void *ctx;
    float *scratch;
    uint32_t scratchCapacity;
    double sampleRate;
} JayTap;

// Error codes handed back to Rust. Negative so they cannot collide with an
// OSStatus, which callers may want to see verbatim.
enum {
    kJayTapOK = 0,
    kJayTapUnsupportedOS = -1,
    kJayTapAllocFailed = -2,
    kJayTapNoOutputDevice = -3,
};

// UID of the current default output device, which the aggregate device needs
// as its main sub-device. Returns nil if there is no output device at all.
static NSString *JayDefaultOutputUID(void) {
    AudioObjectID device = kAudioObjectUnknown;
    UInt32 size = sizeof(device);
    AudioObjectPropertyAddress deviceAddress = {
        .mSelector = kAudioHardwarePropertyDefaultOutputDevice,
        .mScope = kAudioObjectPropertyScopeGlobal,
        .mElement = kAudioObjectPropertyElementMain,
    };
    if (AudioObjectGetPropertyData(kAudioObjectSystemObject, &deviceAddress, 0, NULL, &size,
                                   &device) != noErr
        || device == kAudioObjectUnknown) {
        return nil;
    }

    CFStringRef uid = NULL;
    size = sizeof(uid);
    AudioObjectPropertyAddress uidAddress = {
        .mSelector = kAudioDevicePropertyDeviceUID,
        .mScope = kAudioObjectPropertyScopeGlobal,
        .mElement = kAudioObjectPropertyElementMain,
    };
    if (AudioObjectGetPropertyData(device, &uidAddress, 0, NULL, &size, &uid) != noErr || !uid) {
        return nil;
    }
    return (__bridge_transfer NSString *)uid;
}

static OSStatus JayTapIOProc(AudioObjectID inDevice,
                             const AudioTimeStamp *inNow,
                             const AudioBufferList *inInputData,
                             const AudioTimeStamp *inInputTime,
                             AudioBufferList *outOutputData,
                             const AudioTimeStamp *inOutputTime,
                             void *inClientData) {
    (void)inDevice;
    (void)inNow;
    (void)inInputTime;
    (void)outOutputData;
    (void)inOutputTime;

    JayTap *tap = (JayTap *)inClientData;
    if (!tap || !tap->callback || !inInputData || inInputData->mNumberBuffers == 0) {
        return noErr;
    }

    const AudioBuffer *first = &inInputData->mBuffers[0];
    if (first->mDataByteSize == 0 || first->mData == NULL) {
        return noErr;
    }

    if (inInputData->mNumberBuffers > 1) {
        // Planar: one buffer per channel, each holding `frames` samples.
        uint32_t frames = first->mDataByteSize / sizeof(float);
        if (frames > tap->scratchCapacity) {
            frames = tap->scratchCapacity;
        }
        for (uint32_t f = 0; f < frames; f++) {
            tap->scratch[f] = 0.0f;
        }
        for (UInt32 b = 0; b < inInputData->mNumberBuffers; b++) {
            const float *src = (const float *)inInputData->mBuffers[b].mData;
            if (!src) continue;
            for (uint32_t f = 0; f < frames; f++) {
                tap->scratch[f] += src[f];
            }
        }
        const float scale = 1.0f / (float)inInputData->mNumberBuffers;
        for (uint32_t f = 0; f < frames; f++) {
            tap->scratch[f] *= scale;
        }
        tap->callback(tap->ctx, tap->scratch, frames);
    } else {
        // Interleaved: one buffer holding `frames * channels` samples.
        UInt32 channels = first->mNumberChannels ? first->mNumberChannels : 1;
        uint32_t total = first->mDataByteSize / sizeof(float);
        uint32_t frames = total / channels;
        if (frames > tap->scratchCapacity) {
            frames = tap->scratchCapacity;
        }
        const float *src = (const float *)first->mData;
        const float scale = 1.0f / (float)channels;
        for (uint32_t f = 0; f < frames; f++) {
            float sum = 0.0f;
            for (UInt32 c = 0; c < channels; c++) {
                sum += src[f * channels + c];
            }
            tap->scratch[f] = sum * scale;
        }
        tap->callback(tap->ctx, tap->scratch, frames);
    }

    return noErr;
}

// Start a global system-audio tap.
//
// On success returns 0, writes the opaque handle to `outHandle` and the device
// sample rate to `outSampleRate`. On failure returns a negative code from the
// enum above, or a CoreAudio OSStatus.
int jay_system_tap_start(JayTapCallback callback,
                         void *ctx,
                         void **outHandle,
                         double *outSampleRate) {
    if (@available(macOS 14.4, *)) {
        // fall through
    } else {
        return kJayTapUnsupportedOS;
    }

    if (!callback || !outHandle) {
        return kJayTapAllocFailed;
    }

    JayTap *tap = (JayTap *)calloc(1, sizeof(JayTap));
    if (!tap) {
        return kJayTapAllocFailed;
    }
    tap->callback = callback;
    tap->ctx = ctx;
    tap->tap = kAudioObjectUnknown;
    tap->aggregate = kAudioObjectUnknown;

    // Generous: IOProc buffer sizes are typically 512 or 1024 frames.
    tap->scratchCapacity = 16384;
    tap->scratch = (float *)calloc(tap->scratchCapacity, sizeof(float));
    if (!tap->scratch) {
        free(tap);
        return kJayTapAllocFailed;
    }

    @autoreleasepool {
        // A global tap: everything the machine is playing. Excluding no
        // processes, and unmuted, so the user still hears their own call.
        CATapDescription *description =
            [[CATapDescription alloc] initStereoGlobalTapButExcludeProcesses:@[]];
        description.name = @"jay system audio";
        description.UUID = [NSUUID UUID];
        description.muteBehavior = CATapUnmuted;
        description.privateTap = YES;

        OSStatus status = AudioHardwareCreateProcessTap(description, &tap->tap);
        if (status != noErr) {
            free(tap->scratch);
            free(tap);
            return (int)status;
        }

        // The aggregate device needs a real device as its main sub-device.
        // Without one it still clocks and still calls the IOProc, but every
        // sample arrives as zero, which is a uniquely unhelpful failure: it
        // looks exactly like a working tap in a silent room.
        NSString *outputUID = JayDefaultOutputUID();
        if (!outputUID) {
            AudioHardwareDestroyProcessTap(tap->tap);
            free(tap->scratch);
            free(tap);
            return kJayTapNoOutputDevice;
        }

        NSString *aggregateUID = [[NSUUID UUID] UUIDString];
        NSDictionary *aggregate = @{
            @kAudioAggregateDeviceNameKey : @"jay aggregate",
            @kAudioAggregateDeviceUIDKey : aggregateUID,
            @kAudioAggregateDeviceMainSubDeviceKey : outputUID,
            @kAudioAggregateDeviceIsPrivateKey : @YES,
            @kAudioAggregateDeviceIsStackedKey : @NO,
            @kAudioAggregateDeviceTapAutoStartKey : @YES,
            @kAudioAggregateDeviceSubDeviceListKey : @[ @{
                @kAudioSubDeviceUIDKey : outputUID,
            } ],
            @kAudioAggregateDeviceTapListKey : @[ @{
                @kAudioSubTapUIDKey : [description.UUID UUIDString],
                @kAudioSubTapDriftCompensationKey : @YES,
            } ],
        };

        status = AudioHardwareCreateAggregateDevice((__bridge CFDictionaryRef)aggregate,
                                                    &tap->aggregate);
        if (status != noErr) {
            AudioHardwareDestroyProcessTap(tap->tap);
            free(tap->scratch);
            free(tap);
            return (int)status;
        }

        // Ask the tap what rate it is running at, so Rust can resample
        // correctly rather than assuming 48 kHz and being subtly wrong.
        AudioStreamBasicDescription format = {0};
        UInt32 size = sizeof(format);
        AudioObjectPropertyAddress formatAddress = {
            .mSelector = kAudioTapPropertyFormat,
            .mScope = kAudioObjectPropertyScopeGlobal,
            .mElement = kAudioObjectPropertyElementMain,
        };
        if (AudioObjectGetPropertyData(tap->tap, &formatAddress, 0, NULL, &size, &format) == noErr
            && format.mSampleRate > 0) {
            tap->sampleRate = format.mSampleRate;
        } else {
            tap->sampleRate = 48000.0;
        }

        status = AudioDeviceCreateIOProcID(tap->aggregate, JayTapIOProc, tap, &tap->ioProc);
        if (status != noErr) {
            AudioHardwareDestroyAggregateDevice(tap->aggregate);
            AudioHardwareDestroyProcessTap(tap->tap);
            free(tap->scratch);
            free(tap);
            return (int)status;
        }

        status = AudioDeviceStart(tap->aggregate, tap->ioProc);
        if (status != noErr) {
            AudioDeviceDestroyIOProcID(tap->aggregate, tap->ioProc);
            AudioHardwareDestroyAggregateDevice(tap->aggregate);
            AudioHardwareDestroyProcessTap(tap->tap);
            free(tap->scratch);
            free(tap);
            return (int)status;
        }
    }

    if (outSampleRate) {
        *outSampleRate = tap->sampleRate;
    }
    *outHandle = tap;
    return kJayTapOK;
}

void jay_system_tap_stop(void *handle) {
    JayTap *tap = (JayTap *)handle;
    if (!tap) {
        return;
    }

    if (tap->ioProc) {
        AudioDeviceStop(tap->aggregate, tap->ioProc);
        AudioDeviceDestroyIOProcID(tap->aggregate, tap->ioProc);
    }
    if (tap->aggregate != kAudioObjectUnknown) {
        AudioHardwareDestroyAggregateDevice(tap->aggregate);
    }
    if (tap->tap != kAudioObjectUnknown) {
        AudioHardwareDestroyProcessTap(tap->tap);
    }
    free(tap->scratch);
    free(tap);
}

// ---------------------------------------------------------------------------
// Screen capture, in process.
//
// jay first shelled out to /usr/sbin/screencapture, which works from a
// terminal and fails from the app with "could not create image from display"
// even with Screen Recording granted and toggled on. TCC evaluates the request
// against the process that makes it, and a spawned Apple binary does not
// cleanly inherit the parent's grant. Capturing here makes the request
// unambiguously jay's.
// ---------------------------------------------------------------------------

#import <CoreGraphics/CoreGraphics.h>
#import <ImageIO/ImageIO.h>

enum {
    kJayShotOK = 0,
    kJayShotNoImage = -1,   // almost always Screen Recording permission
    kJayShotWriteFailed = -2,
};

int jay_capture_main_display(const char *path) {
    if (!path) {
        return kJayShotWriteFailed;
    }

    @autoreleasepool {
        CGImageRef image = CGDisplayCreateImage(CGMainDisplayID());
        if (!image) {
            // Returns NULL rather than erroring when TCC has declined.
            return kJayShotNoImage;
        }

        NSURL *url = [NSURL fileURLWithPath:[NSString stringWithUTF8String:path]];
        CGImageDestinationRef dest = CGImageDestinationCreateWithURL(
            (__bridge CFURLRef)url, (__bridge CFStringRef) @"public.png", 1, NULL);
        if (!dest) {
            CGImageRelease(image);
            return kJayShotWriteFailed;
        }

        CGImageDestinationAddImage(dest, image, NULL);
        bool ok = CGImageDestinationFinalize(dest);
        CFRelease(dest);
        CGImageRelease(image);

        return ok ? kJayShotOK : kJayShotWriteFailed;
    }
}
