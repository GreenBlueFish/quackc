// quack_event.hpp — event-driven (Mode B) simulation primitives for the Quack engine.
//
// This header is a supporting artifact for the event-driven simulation path (Requirement 6). The
// primary Mode B engine is implemented in Rust (`src/sim_event.rs`), which interprets the netlist
// IR directly over discrete integer ticks. This header provides a minimal, standard-library-only
// C++17 scaffold that mirrors that engine's model, so the generated/event C++ path can share the
// same vocabulary (tick type, INF sentinel, event, and an ascending-tick event queue).
//
// It depends only on the C++17 standard library and the spike-time definitions in quack_rt.hpp
// (SpikeTime, INF). It intentionally does NOT re-derive the full engine in C++: the authoritative
// semantics (inc at t+n, min first-arrival, lt inhibit-at-tick<=t blocking, per-tick topological
// resolution with no delta cycles, run-until-empty-or-max-tick, non-firing -> INF) live in the
// Rust engine and are documented there.
//
//   Requirements: 6.1 (discrete ticks + ascending-tick event queue),
//                 6.8 (run until queue empty or a configurable maximum tick),
//                 6.9 (a non-firing node resolves to INF).

#pragma once

#include <cstdint>
#include <queue>
#include <vector>

#include "quack_rt.hpp"

namespace quack {

// A discrete simulation tick. Ticks are non-negative integers starting at 0 (Req 6.1). We use a
// 64-bit tick so a configurable maximum tick can exceed the SpikeTime finite range without
// wrapping; a spike *time* that is finite still fits in SpikeTime.
using Tick = std::uint64_t;

// The default maximum tick for a run when none is configured: a positive integer bound so a run
// always terminates even if events keep being produced (Req 6.8). Mirrors the Rust engine's
// DEFAULT_MAX_TICK.
constexpr Tick DEFAULT_MAX_TICK = 1u << 20; // 1,048,576 ticks

// One scheduled event: a node (by its IR node id) is told to fire at a given tick (Req 6.1, 6.2).
// Node ids match the netlist IR's u32 NodeId.
struct Event {
    Tick tick;             // the tick at which this event occurs
    std::uint32_t node_id; // the IR node id this event targets
};

// Order events by ascending tick (Req 6.1). This is the comparator for a min-heap: std::priority_queue
// is a max-heap by default, so we invert the comparison to pop the smallest tick first. Ties on tick
// are broken by node id so ordering is deterministic; within a tick the engine resolves nodes in
// topological order regardless (Req 6.7), so the tie-break here only affects queue draining order.
struct EventLater {
    bool operator()(const Event& a, const Event& b) const {
        if (a.tick != b.tick) {
            return a.tick > b.tick;
        }
        return a.node_id > b.node_id;
    }
};

// An event queue ordered by ascending tick (Req 6.1): popping yields the earliest-tick event first.
using EventQueue = std::priority_queue<Event, std::vector<Event>, EventLater>;

// Schedule a node to fire at the given tick (Req 6.2, 6.3). A convenience wrapper over the queue.
inline void schedule(EventQueue& q, std::uint32_t node_id, Tick tick) {
    q.push(Event{tick, node_id});
}

// Whether a run should stop: the queue is empty, or the next event is beyond the maximum tick
// (Req 6.8). When this is true, every node that has not fired is assigned INF by the caller
// (Req 6.9).
inline bool run_done(const EventQueue& q, Tick max_tick) {
    return q.empty() || q.top().tick > max_tick;
}

} // namespace quack
