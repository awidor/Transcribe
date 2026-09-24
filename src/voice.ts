// Moves `value` toward `goal` at `rate` per second, independent of frame rate.
export function toward(value: number, goal: number, rate: number, dt: number) {
  return value + (goal - value) * (1 - Math.exp(-rate * dt));
}

// Decibels a sound has to clear the room's noise by before it counts.
const GATE = 8;
// Decibels above the noise that full height takes, at the least.
const RANGE = 24;
// Seconds of sound the room's noise is judged from.
const WINDOW = 2;

// Loudness from 0 to 1, judged against this microphone's own room: quiet and
// loud microphones both reach the top, while background noise stays near zero.
// Readings of zero (before the microphone reports) are ignored.
export function loudness() {
  const recent: { at: number; db: number }[] = [];
  let now = 0;
  let floor: number | null = null;
  let peak = 0;
  let loud = 0;
  return (level: number, dt: number) => {
    now += dt;
    let target = 0;
    if (level > 0) {
      const db = 20 * Math.log10(Math.max(level, 1e-6));
      recent.push({ at: now, db });
      while (recent[0].at < now - WINDOW) recent.shift();
      // A quiet moment of the last two seconds is the room's noise; pauses
      // between words keep it there even while someone talks.
      const sorted = recent.map((sample) => sample.db).sort((a, b) => a - b);
      const quiet = sorted[Math.floor((sorted.length - 1) * 0.2)];
      if (floor === null) {
        floor = db;
        peak = db + RANGE;
      }
      floor = toward(floor, quiet, 4, dt);
      const ceiling = Math.max(db, floor + RANGE);
      peak = toward(peak, ceiling, ceiling > peak ? 14 : 0.8, dt);
      const clear = (db - floor - GATE) / (peak - floor - GATE);
      target = Math.min(1, Math.max(0, clear)) ** 0.7;
    }
    loud = toward(loud, target, target > loud ? 12 : 8, dt);
    return loud;
  };
}
