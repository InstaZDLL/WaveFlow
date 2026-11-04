// C entry points over Signalsmith Stretch, on interleaved buffers. Declared by
// hand in `mod.rs`. Nothing may unwind into Rust: what allocates is caught and
// reported.

#include <signalsmith-stretch.h>

#include <cstddef>
#include <new>
#include <stdexcept>

#ifdef _MSC_VER
// What `std::vector` calls when asked for more than it can ever hold, and the
// one thing it needs from MSVCP140.dll, which the app must not depend on: see
// `build.rs`. Thrown like the original, into the catches below.
namespace std {
[[noreturn]] void __cdecl _Xlength_error(const char *message) { throw length_error(message); }
} // namespace std
#endif

namespace {

// `view[channel][frame]` over an interleaved buffer.
template <typename Sample>
class Interleaved {
  Sample *data_;
  int channels_;

public:
  Interleaved(Sample *data, int channels) : data_(data), channels_(channels) {}

  class Channel {
    Sample *data_;
    int channel_;
    int stride_;

  public:
    Channel(Sample *data, int channel, int stride)
        : data_(data), channel_(channel), stride_(stride) {}
    Sample &operator[](std::size_t frame) const {
      return data_[frame * stride_ + channel_];
    }
  };

  Channel operator[](int channel) const {
    return Channel(data_, channel, channels_);
  }
};

} // namespace

struct WfStretch {
  // A fixed seed: the same input gives the same output, and no
  // `std::random_device` to fail in a sandbox.
  signalsmith::stretch::SignalsmithStretch<float> inner{0x57524554L};
  int channels = 0;
};

extern "C" {

WfStretch *wf_stretch_new(int channels, std::size_t block, std::size_t interval) noexcept {
  try {
    auto *handle = new WfStretch;
    handle->channels = channels;
    handle->inner.configure(channels, block, interval);
    return handle;
  } catch (...) {
    return nullptr;
  }
}

void wf_stretch_free(WfStretch *handle) noexcept { delete handle; }

bool wf_stretch_reset(WfStretch *handle) noexcept {
  try {
    handle->inner.reset();
    return true;
  } catch (...) {
    return false;
  }
}

std::size_t wf_stretch_input_latency(const WfStretch *handle) noexcept {
  return static_cast<std::size_t>(handle->inner.inputLatency());
}

std::size_t wf_stretch_output_latency(const WfStretch *handle) noexcept {
  return static_cast<std::size_t>(handle->inner.outputLatency());
}

bool wf_stretch_seek(WfStretch *handle, const float *input, std::size_t frames,
                     double rate) noexcept {
  try {
    handle->inner.seek(Interleaved<const float>(input, handle->channels), static_cast<int>(frames),
                       rate);
    return true;
  } catch (...) {
    return false;
  }
}

bool wf_stretch_process(WfStretch *handle, const float *input, std::size_t input_frames,
                        float *output, std::size_t output_frames) noexcept {
  try {
    handle->inner.process(Interleaved<const float>(input, handle->channels),
                          static_cast<int>(input_frames),
                          Interleaved<float>(output, handle->channels),
                          static_cast<int>(output_frames));
    return true;
  } catch (...) {
    return false;
  }
}

bool wf_stretch_flush(WfStretch *handle, float *output, std::size_t frames) noexcept {
  try {
    handle->inner.flush(Interleaved<float>(output, handle->channels), static_cast<int>(frames));
    return true;
  } catch (...) {
    return false;
  }
}

} // extern "C"
