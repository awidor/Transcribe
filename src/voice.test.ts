import { describe, expect, it } from 'vitest';
import { loudness } from './voice';

// Feeds `level` at 60 frames per second and returns every reading.
function feed(
  meter: ReturnType<typeof loudness>,
  level: (frame: number) => number,
  seconds: number,
) {
  const readings = [];
  for (let frame = 0; frame < seconds * 60; frame++) readings.push(meter(level(frame), 1 / 60));
  return readings;
}
const last = (readings: number[]) => readings[readings.length - 1];
const highest = (readings: number[]) => Math.max(...readings);
const decibels = (db: number) => 10 ** (db / 20);
// Deterministic ±3 dB jitter, like a room's steady noise.
const jitter = (frame: number) => Math.sin(frame * 12.9898) * 3;

describe('loudness', () => {
  it('fills the range for a quiet microphone', () => {
    const meter = loudness();
    feed(meter, (frame) => decibels(-80 + jitter(frame)), 2);
    expect(last(feed(meter, () => decibels(-58), 0.4))).toBeGreaterThan(0.7);
  });
  it('fills the range for a loud microphone', () => {
    const meter = loudness();
    feed(meter, (frame) => decibels(-55 + jitter(frame)), 2);
    expect(last(feed(meter, () => decibels(-12), 0.4))).toBeGreaterThan(0.8);
  });
  it('stays still on steady background noise', () => {
    const meter = loudness();
    expect(
      highest(feed(meter, (frame) => decibels(-65 + jitter(frame)), 4).slice(60)),
    ).toBeLessThan(0.05);
  });
  it('barely moves for brief noises', () => {
    const meter = loudness();
    // A knock or keystroke 12 dB over the room every half second.
    const readings = feed(
      meter,
      (frame) => decibels(-65 + jitter(frame) + (frame % 30 < 2 ? 12 : 0)),
      4,
    );
    expect(highest(readings.slice(60))).toBeLessThan(0.25);
  });
  it('learns a loud steady noise such as a fan', () => {
    const meter = loudness();
    feed(meter, (frame) => decibels(-70 + jitter(frame)), 1);
    expect(last(feed(meter, (frame) => decibels(-42 + jitter(frame)), 4))).toBeLessThan(0.05);
  });
  it('ignores readings from before the microphone starts', () => {
    const meter = loudness();
    feed(meter, () => 0, 1);
    expect(
      highest(feed(meter, (frame) => decibels(-65 + jitter(frame)), 3).slice(60)),
    ).toBeLessThan(0.05);
  });
  it('falls back after speech stops', () => {
    const meter = loudness();
    feed(meter, (frame) => decibels(-70 + jitter(frame)), 2);
    feed(meter, () => decibels(-30), 0.5);
    expect(last(feed(meter, (frame) => decibels(-70 + jitter(frame)), 0.6))).toBeLessThan(0.1);
  });
});
