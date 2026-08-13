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

        // What rate the frames actually arrive at, so Rust can resample
        // correctly rather than assuming 48 kHz and being subtly wrong.
        //
        // Ask the *aggregate device*, not the tap. The tap has a format of its
        // own and cheerfully reports 48 kHz while the IOProc below is fed by the
        // aggregate, which follows its main sub-device. Bluetooth headphones
        // that are also the default input run the hands-free profile at 16 kHz
        // mono, and the difference is not subtle: taking the tap's 48 kHz meant
        // resampling 16 kHz audio as though it were 48 kHz, which discards two
        // samples in three and drops what survives an octave and a half. It
        // measured as exactly 114 delivered frames of an expected 343, twice,
        // and it made the other person unintelligible rather than merely quiet.
        Float64 deviceRate = 0.0;
        UInt32 size = sizeof(deviceRate);
        AudioObjectPropertyAddress rateAddress = {
            .mSelector = kAudioDevicePropertyNominalSampleRate,
            .mScope = kAudioObjectPropertyScopeGlobal,
            .mElement = kAudioObjectPropertyElementMain,
        };
        if (AudioObjectGetPropertyData(tap->aggregate, &rateAddress, 0, NULL, &size, &deviceRate)
                == noErr
            && deviceRate > 0) {
            tap->sampleRate = deviceRate;
        } else {
            // Fall back to the tap's own idea, then to the common case.
            AudioStreamBasicDescription format = {0};
            UInt32 formatSize = sizeof(format);
            AudioObjectPropertyAddress formatAddress = {
                .mSelector = kAudioTapPropertyFormat,
                .mScope = kAudioObjectPropertyScopeGlobal,
                .mElement = kAudioObjectPropertyElementMain,
            };
            if (AudioObjectGetPropertyData(tap->tap, &formatAddress, 0, NULL, &formatSize, &format)
                    == noErr
                && format.mSampleRate > 0) {
                tap->sampleRate = format.mSampleRate;
            } else {
                tap->sampleRate = 48000.0;
            }
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

// Longest edge of a written capture, in pixels.
//
// A Retina grab of this display is ~3456 wide and lands at 8.5 MB, and the
// model's high-resolution tier caps at 2576 on the long edge — so everything
// above that is upload time spent on pixels that are thrown away before
// anything reads them. 1800 stays comfortably legible for code while cutting
// the file by roughly three quarters.
static const size_t kJayShotMaxEdge = 1800;

// Scale down, preserving aspect. Returns a retained image the caller frees, or
// the original retained if it is already small enough.
static CGImageRef JayScaleToFit(CGImageRef image) {
    size_t w = CGImageGetWidth(image);
    size_t h = CGImageGetHeight(image);
    size_t longest = w > h ? w : h;
    if (longest <= kJayShotMaxEdge) {
        return CGImageRetain(image);
    }

    double factor = (double)kJayShotMaxEdge / (double)longest;
    size_t tw = (size_t)(w * factor);
    size_t th = (size_t)(h * factor);

    CGColorSpaceRef space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
    CGContextRef ctx = CGBitmapContextCreate(NULL, tw, th, 8, 0, space,
                                             kCGImageAlphaPremultipliedFirst
                                                 | kCGBitmapByteOrder32Little);
    CGColorSpaceRelease(space);
    if (!ctx) {
        return CGImageRetain(image);
    }

    // Text is the whole point of these captures, so interpolate properly.
    CGContextSetInterpolationQuality(ctx, kCGInterpolationHigh);
    CGContextDrawImage(ctx, CGRectMake(0, 0, tw, th), image);
    CGImageRef scaled = CGBitmapContextCreateImage(ctx);
    CGContextRelease(ctx);

    return scaled ? scaled : CGImageRetain(image);
}

// Human-readable name of the current default output device, for the panel.
//
// The tap binds to whatever this device is at the moment jay starts and never
// looks again, so naming it is the difference between "the other person is
// silent" and "jay is listening to the laptop speakers while your call is in
// your headphones". Writes at most `len` bytes including the terminator and
// returns 0 on success.
int jay_default_output_name(char *buffer, size_t len) {
    if (!buffer || len == 0) {
        return -1;
    }
    buffer[0] = '\0';

    @autoreleasepool {
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
            return -1;
        }

        CFStringRef name = NULL;
        size = sizeof(name);
        AudioObjectPropertyAddress nameAddress = {
            .mSelector = kAudioObjectPropertyName,
            .mScope = kAudioObjectPropertyScopeGlobal,
            .mElement = kAudioObjectPropertyElementMain,
        };
        if (AudioObjectGetPropertyData(device, &nameAddress, 0, NULL, &size, &name) != noErr
            || !name) {
            return -1;
        }

        NSString *readable = (__bridge_transfer NSString *)name;
        return [readable getCString:buffer maxLength:len encoding:NSUTF8StringEncoding] ? 0 : -1;
    }
}

int jay_capture_main_display(const char *path) {
    if (!path) {
        return kJayShotWriteFailed;
    }

    @autoreleasepool {
        CGImageRef full = CGDisplayCreateImage(CGMainDisplayID());
        if (!full) {
            // Returns NULL rather than erroring when TCC has declined.
            return kJayShotNoImage;
        }
        CGImageRef image = JayScaleToFit(full);
        CGImageRelease(full);

        NSURL *url = [NSURL fileURLWithPath:[NSString stringWithUTF8String:path]];
        // JPEG, not PNG. PNG is lossless, which matters for pixel work and not
        // at all for a model reading a stack trace — and it is roughly ten
        // times the bytes. At 0.85 the text stays crisp and the upload stops
        // being the slowest part of pressing the button.
        CGImageDestinationRef dest = CGImageDestinationCreateWithURL(
            (__bridge CFURLRef)url, (__bridge CFStringRef) @"public.jpeg", 1, NULL);
        if (!dest) {
            CGImageRelease(image);
            return kJayShotWriteFailed;
        }

        NSDictionary *options = @{(__bridge NSString *)kCGImageDestinationLossyCompressionQuality: @0.85};
        CGImageDestinationAddImage(dest, image, (__bridge CFDictionaryRef)options);
        bool ok = CGImageDestinationFinalize(dest);
        CFRelease(dest);
        CGImageRelease(image);

        return ok ? kJayShotOK : kJayShotWriteFailed;
    }
}
