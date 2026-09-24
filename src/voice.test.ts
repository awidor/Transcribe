import { describe, expect, it } from 'vitest';
import { loudness } from './voice';

// Feeds `level` at 60 frames per second and returns the last reading.
function feed(
  meter: ReturnType<typeof loudness>,
  level: (frame: number) => number,
  seconds: number,
) {
  let loud = 0;
  for (let frame = 0; frame < seconds * 60; frame++) loud = meter(level(frame), 1 / 60);
  return loud;
}

describe('loudness', () => {
  it('fills the range for a quiet microphone', () => {
    const meter = loudness();
    feed(meter, () => 1e-4, 1.5);
    expect(feed(meter, () => 1e-3, 0.3)).toBeGreaterThan(0.8);
  });
  it('fills the range for a loud microphone without clipping on noise', () => {
    const meter = loudness();
    expect(feed(meter, () => 3e-3, 1.5)).toBeLessThan(0.15);
    expect(feed(meter, () => 0.3, 0.3)).toBeGreaterThan(0.8);
  });
  it('stays near zero on steady background noise', () => {
    const meter = loudness();
    expect(feed(meter, (frame) => (frame % 7 < 3 ? 1e-4 : 1.4e-4), 3)).toBeLessThan(0.15);
  });
  it('falls back after speech stops', () => {
    const meter = loudness();
    feed(meter, () => 1e-4, 1.5);
    feed(meter, () => 1e-3, 0.5);
    expect(feed(meter, () => 1e-4, 0.6)).toBeLessThan(0.1);
  });
});
