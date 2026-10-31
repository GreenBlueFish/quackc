# Lesson 01 — Spike times and `inf`

**Goal:** understand what a *value* is in Quack before writing any logic. This is the conceptual
foundation; every later lesson rests on it.

## Concept: a value is a time, not a number

Every wire carries a single **spike time**: the tick at which a spike occurs. A value is one of:

- a non-negative integer `t` (`0, 1, 2, ...`) meaning "a spike arrived at tick `t`", or
- `inf` meaning "no spike ever arrives".

That is the whole value space. There is no notion of magnitude, voltage, or amplitude — only
*when*. When you read `a = 3`, read it as "the spike on `a` arrives at tick 3", not "a equals 3".

## Concept: `inf` is a real, usable value

`inf` is the "never fires" value and behaves as the largest possible time. It is the default for
any input you do not drive in a testbench (you will use this constantly in Lesson 04). Delaying
`inf` leaves it `inf` (a spike that never comes, pushed later, still never comes).

## Concept: why there are no ordinary numeric constants in wire logic

You can write `3` as *stimulus* in a testbench, but you cannot assign a bare number like `y = 3;`
as hardware. The reason is **time-shift invariance**: if every input spike shifts later by some
constant `c`, every output must shift by exactly `c` too. A hard-coded tick like `3` would be an
absolute time reference that breaks that property. The only numeric literal allowed in wire logic
is as a *delay amount* (`a + 2`), which is relative, not absolute.

Hold onto invariance — Lesson 12 checks it automatically with `--check-invariance`.

## No new files required

This lesson is intuition, not code. If you want to make it concrete, extend `hello.sta` from
Lesson 00 with a wave where `a = inf` and confirm the passthrough output is `inf`.

## Check your understanding

1. If `a` arrives at tick 5 and the whole system is shifted so every input arrives 10 ticks later,
   when does `a` arrive? When does an `inf` input arrive after the same shift?
2. Why is `y = min(a, b) + 1;` legal but `y = 7;` is not?
3. In plain words, what does `inf` mean for an output wire of a real design?