// quack_rt.hpp — hand-written runtime support for the Quack functional C++ backend (Mode A).
//
// This header is shipped alongside the C++ sources emitted by `quackc`. Generated code depends
// only on the C++17 standard library and this header (Requirements 5.11, 15.4).
//
// It defines the spike-time scalar type, the "never spikes" sentinel INF, and the three saturating
// primitive helpers (st_inc / st_min / st_lt) that generated code lowers primitive operators to,
// plus a printing helper that renders INF as the text "inf".
//
//   Requirements: 5.7, 5.8, 5.9, 5.10
//
// SpikeTime width selection (Requirement 5.8 / 5.9):
//   - define QUACK_TIME_WIDTH to 16 to use uint16_t (INF == 65535)
//   - define QUACK_TIME_WIDTH to 32, or leave undefined, to use uint32_t (INF == 4294967295)

#pragma once

#include <cstdint>
#include <limits>
#include <algorithm>
#include <ostream>
#include <string>

#ifndef QUACK_TIME_WIDTH
#define QUACK_TIME_WIDTH 32
#endif

#if QUACK_TIME_WIDTH == 16
namespace quack {
using SpikeTime = std::uint16_t; // INF == 65535
}
#elif QUACK_TIME_WIDTH == 32
namespace quack {
using SpikeTime = std::uint32_t; // INF == 4294967295
}
#else
#error "QUACK_TIME_WIDTH must be 16 or 32"
#endif

namespace quack {

// INF is the maximum value of the chosen SpikeTime type and means "no spike ever occurs"
// (Requirements 5.8, 5.9). For uint32_t this is 4294967295; for uint16_t it is 65535.
constexpr SpikeTime INF = std::numeric_limits<SpikeTime>::max();

// st_inc — unit-delay / increment with saturating arithmetic (Requirements 5.9... see 5.10).
//   - INF stays INF (an absent spike, shifted, is still absent).
//   - Any increment whose result would exceed the type maximum clamps to INF.
// The comparison is done in a width large enough to hold INF + n without wrapping.
inline SpikeTime st_inc(SpikeTime a, unsigned n = 1) {
    if (a == INF) {
        return INF;
    }
    // Promote to a 64-bit accumulator so the sum cannot wrap before we test it.
    const std::uint64_t sum = static_cast<std::uint64_t>(a) + static_cast<std::uint64_t>(n);
    if (sum >= static_cast<std::uint64_t>(INF)) {
        return INF;
    }
    return static_cast<SpikeTime>(sum);
}

// st_min — first arrival. Because INF is the type maximum, a plain minimum is already correct:
// a finite spike always beats an absent one (Requirement 5.7).
inline SpikeTime st_min(SpikeTime a, SpikeTime b) {
    return std::min(a, b);
}

// st_lt — strict less-than / inhibit. Returns `a` if `a < b`, otherwise INF. Ties (a == b) block,
// yielding INF (Requirement 5.7).
inline SpikeTime st_lt(SpikeTime a, SpikeTime b) {
    return a < b ? a : INF;
}

// Printing helpers: render INF as the string "inf" and finite values as their integer (Req 5.7).
inline std::string st_to_string(SpikeTime a) {
    return a == INF ? std::string("inf") : std::to_string(static_cast<std::uint64_t>(a));
}

inline std::ostream& st_print(std::ostream& os, SpikeTime a) {
    if (a == INF) {
        return os << "inf";
    }
    return os << static_cast<std::uint64_t>(a);
}

} // namespace quack
