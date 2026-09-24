// Moves `value` toward `goal` at `rate` per second, independent of frame rate.
export function toward(value: number, goal: number, rate: number, dt: number) {
  return value + (goal - value) * (1 - Math.exp(-rate * dt));
}

// Loudness from 0 to 1, judged against this microphone's own range: the
// quietest recent sound sets the floor and recent speech the ceiling, so quiet
// and loud microphones both reach the top. Steady noise stays near zero.
export function loudness() {
  let floor = -50;
  let peak = -32;
  let loud = 0;
  return (level: number, dt: number) => {
    const db = 20 * Math.log10(Math.max(level, 1e-5));
    floor = toward(floor, db, db < floor ? 6 : 0.08, dt);
    // The ceiling jumps up to louder speech and sinks back toward quieter
    // speech, but always keeps some range above the floor.
    const ceiling = Math.max(db, floor + 18);
    peak = toward(peak, ceiling, ceiling > peak ? 14 : 1.5, dt);
    const target = Math.min(1, Math.max(0, (db - floor - 3) / (peak - floor - 3)));
    loud = toward(loud, target, target > loud ? 28 : 7, dt);
    return loud;
  };
}
