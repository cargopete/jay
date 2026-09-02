// Microphone capture through Apple's voice-processing unit, which cancels the
// room.
//
// The plain cpal input path hears whatever the speakers are playing and hands
// it to the VAD as speech. Measured on a real 19-minute call: the microphone
// sat at 0.0408 RMS against a documented speech level of about 0.02, never fell
// silent, and so every microphone utterance ran to the segmenter's 25-second
// cap and chopped mid-sentence. Nineteen of them, in one meeting. The text-side
// echo guard cannot repair that, because the two channels no longer agree about
// where sentences begin.
//
// `kAudioUnitSubType_VoiceProcessingIO` is the same acoustic echo canceller
// every call application on this platform uses. It is a duplex unit: bus 1 is
// the microphone, bus 0 is the speaker, and it subtracts the second from the
// first. jay renders silence on bus 0 because jay is not the one playing the
// call — that is Zoom, or a browser — and whether the unit still references the
// device's output mix is precisely the question this file exists to answer.
// `bypass` runs the identical path with the cancellation switched off, so the
// two can be measured against each other rather than argued about.
//
// Requires macOS 10.7 for the unit itself, which is to say it is always there.

#import <AudioToolbox/AudioToolbox.h>
#import <CoreAudio/CoreAudio.h>
#import <Foundation/Foundation.h>

typedef void (*JayVoiceMicCallback)(void *ctx, const float *mono, uint32_t frames);

typedef struct JayVoiceMic {
    AudioUnit unit;
    JayVoiceMicCallback callback;
    void *ctx;
    AudioBufferList *bufferList;
    float *scratch;
    uint32_t scratchCapacity;
} JayVoiceMic;

// Negative so they cannot collide with an OSStatus the caller may want verbatim.
enum {
    kJayVoiceMicOK = 0,
    kJayVoiceMicAllocFailed = -2,
    kJayVoiceMicNoComponent = -4,
    kJayVoiceMicNoInputDevice = -5,
};

// The unit is asked for exactly what whisper wants, so there is no resampler on
// this path at all. Voice units are built for 16 kHz; asking for it is the
// normal case rather than a favour.
static const Float64 kJayVoiceMicRate = 16000.0;

// Bus 1 is the microphone, bus 0 is the speaker. CoreAudio calls these
// elements, and confusing them is the traditional way to spend an afternoon.
static const AudioUnitElement kInputBus = 1;
static const AudioUnitElement kOutputBus = 0;

static AudioStreamBasicDescription JayVoiceMicFormat(void) {
    AudioStreamBasicDescription format = {0};
    format.mSampleRate = kJayVoiceMicRate;
    format.mFormatID = kAudioFormatLinearPCM;
    format.mFormatFlags = kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked;
    format.mFramesPerPacket = 1;
    format.mChannelsPerFrame = 1;
    format.mBitsPerChannel = 32;
    format.mBytesPerFrame = 4;
    format.mBytesPerPacket = 4;
    return format;
}

// Bus 0 has to render something. Silence: jay is a listener, and anything it
// put here would come out of the user's speakers in the middle of their call.
static OSStatus JayVoiceMicRenderSilence(void *inRefCon,
                                         AudioUnitRenderActionFlags *ioActionFlags,
                                         const AudioTimeStamp *inTimeStamp,
                                         UInt32 inBusNumber, UInt32 inNumberFrames,
                                         AudioBufferList *ioData) {
    (void)inRefCon;
    (void)inTimeStamp;
    (void)inBusNumber;
    (void)inNumberFrames;

    if (ioData != NULL) {
        for (UInt32 i = 0; i < ioData->mNumberBuffers; i++) {
            memset(ioData->mBuffers[i].mData, 0, ioData->mBuffers[i].mDataByteSize);
        }
    }
    if (ioActionFlags != NULL) {
        *ioActionFlags |= kAudioUnitRenderAction_OutputIsSilence;
    }
    return noErr;
}

// Grow the render scratch if the unit hands us a larger slice than last time.
// Allocation happens here rather than in the render callback where it would be
// a real-time violation; in practice the size settles on the first callback.
static bool JayVoiceMicEnsureCapacity(JayVoiceMic *mic, uint32_t frames) {
    if (frames <= mic->scratchCapacity) {
        return true;
    }
    float *grown = realloc(mic->scratch, (size_t)frames * sizeof(float));
    if (grown == NULL) {
        return false;
    }
    mic->scratch = grown;
    mic->scratchCapacity = frames;
    return true;
}

static OSStatus JayVoiceMicInput(void *inRefCon, AudioUnitRenderActionFlags *ioActionFlags,
                                 const AudioTimeStamp *inTimeStamp, UInt32 inBusNumber,
                                 UInt32 inNumberFrames, AudioBufferList *ioData) {
    (void)ioData;
    JayVoiceMic *mic = (JayVoiceMic *)inRefCon;
    if (mic == NULL || mic->unit == NULL || inNumberFrames == 0) {
        return noErr;
    }
    if (!JayVoiceMicEnsureCapacity(mic, inNumberFrames)) {
        return noErr;
    }

    AudioBufferList list;
    list.mNumberBuffers = 1;
    list.mBuffers[0].mNumberChannels = 1;
    list.mBuffers[0].mDataByteSize = inNumberFrames * sizeof(float);
    list.mBuffers[0].mData = mic->scratch;

    OSStatus status = AudioUnitRender(mic->unit, ioActionFlags, inTimeStamp, inBusNumber,
                                      inNumberFrames, &list);
    if (status != noErr) {
        return status;
    }

    if (mic->callback != NULL) {
        mic->callback(mic->ctx, mic->scratch, inNumberFrames);
    }
    return noErr;
}

// Start the voice-processing microphone.
//
// `bypass` non-zero runs the same unit with echo cancellation disabled, which
// exists so the effect can be measured rather than asserted.
int32_t jay_voice_mic_start(JayVoiceMicCallback callback, void *ctx, int32_t bypass,
                            void **out_handle, int32_t *out_agc_status) {
    if (out_handle == NULL) {
        return kJayVoiceMicAllocFailed;
    }
    *out_handle = NULL;

    AudioComponentDescription description = {
        .componentType = kAudioUnitType_Output,
        .componentSubType = kAudioUnitSubType_VoiceProcessingIO,
        .componentManufacturer = kAudioUnitManufacturer_Apple,
        .componentFlags = 0,
        .componentFlagsMask = 0,
    };

    AudioComponent component = AudioComponentFindNext(NULL, &description);
    if (component == NULL) {
        return kJayVoiceMicNoComponent;
    }

    JayVoiceMic *mic = calloc(1, sizeof(JayVoiceMic));
    if (mic == NULL) {
        return kJayVoiceMicAllocFailed;
    }
    mic->callback = callback;
    mic->ctx = ctx;

    OSStatus status = AudioComponentInstanceNew(component, &mic->unit);
    if (status != noErr) {
        free(mic);
        return status;
    }

    // Input on, output on. The unit is duplex and refuses to initialise with
    // the output side disabled, which is the first thing anyone tries.
    UInt32 enable = 1;
    status = AudioUnitSetProperty(mic->unit, kAudioOutputUnitProperty_EnableIO,
                                  kAudioUnitScope_Input, kInputBus, &enable, sizeof(enable));
    if (status != noErr) {
        goto fail;
    }
    status = AudioUnitSetProperty(mic->unit, kAudioOutputUnitProperty_EnableIO,
                                  kAudioUnitScope_Output, kOutputBus, &enable, sizeof(enable));
    if (status != noErr) {
        goto fail;
    }

    AudioStreamBasicDescription format = JayVoiceMicFormat();

    // What we want handed to us from the microphone bus.
    status = AudioUnitSetProperty(mic->unit, kAudioUnitProperty_StreamFormat,
                                  kAudioUnitScope_Output, kInputBus, &format, sizeof(format));
    if (status != noErr) {
        goto fail;
    }
    // What we promise to render on the speaker bus, which is silence.
    status = AudioUnitSetProperty(mic->unit, kAudioUnitProperty_StreamFormat,
                                  kAudioUnitScope_Input, kOutputBus, &format, sizeof(format));
    if (status != noErr) {
        goto fail;
    }

    AURenderCallbackStruct input = {.inputProc = JayVoiceMicInput, .inputProcRefCon = mic};
    status = AudioUnitSetProperty(mic->unit, kAudioOutputUnitProperty_SetInputCallback,
                                  kAudioUnitScope_Global, kInputBus, &input, sizeof(input));
    if (status != noErr) {
        goto fail;
    }

    AURenderCallbackStruct render = {.inputProc = JayVoiceMicRenderSilence, .inputProcRefCon = mic};
    status = AudioUnitSetProperty(mic->unit, kAudioUnitProperty_SetRenderCallback,
                                  kAudioUnitScope_Input, kOutputBus, &render, sizeof(render));
    if (status != noErr) {
        goto fail;
    }

    // The cancellation itself. Property 2100 is `kAUVoiceIOProperty_BypassVoiceProcessing`.
    UInt32 bypassValue = bypass != 0 ? 1 : 0;
    status = AudioUnitSetProperty(mic->unit, kAUVoiceIOProperty_BypassVoiceProcessing,
                                  kAudioUnitScope_Global, kInputBus, &bypassValue,
                                  sizeof(bypassValue));
    if (status != noErr) {
        goto fail;
    }

    // Automatic gain control left off deliberately. jay's own thresholds are
    // calibrated against real levels — a resting room at 0.003, speech at about
    // 0.02 — and an AGC quietly renormalising the input invalidates every one
    // of them.
    //
    // The element matters and is not the input bus. Set on element 1 this call
    // returns `noErr` and does nothing: a silent room came back at 0.0600 RMS
    // with cancellation on against 0.0016 with it bypassed, which is not a room
    // that got louder, it is a gain control that was never switched off. The
    // status is reported rather than swallowed for the same reason.
    UInt32 agc = 0;
    OSStatus agcStatus =
        AudioUnitSetProperty(mic->unit, kAUVoiceIOProperty_VoiceProcessingEnableAGC,
                             kAudioUnitScope_Global, kOutputBus, &agc, sizeof(agc));
    if (out_agc_status != NULL) {
        *out_agc_status = (int32_t)agcStatus;
    }

    status = AudioUnitInitialize(mic->unit);
    if (status != noErr) {
        goto fail;
    }
    status = AudioOutputUnitStart(mic->unit);
    if (status != noErr) {
        AudioUnitUninitialize(mic->unit);
        goto fail;
    }

    *out_handle = mic;
    return kJayVoiceMicOK;

fail:
    AudioComponentInstanceDispose(mic->unit);
    free(mic->scratch);
    free(mic);
    return status;
}

void jay_voice_mic_stop(void *handle) {
    JayVoiceMic *mic = (JayVoiceMic *)handle;
    if (mic == NULL) {
        return;
    }
    if (mic->unit != NULL) {
        AudioOutputUnitStop(mic->unit);
        AudioUnitUninitialize(mic->unit);
        AudioComponentInstanceDispose(mic->unit);
        mic->unit = NULL;
    }
    free(mic->scratch);
    free(mic);
}
