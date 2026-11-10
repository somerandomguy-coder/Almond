const CROCKFORD = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
let lastMs = 0;
let counter = 0;

/** ULID-shaped, time-sortable id (26 chars). Good enough for mocks and optimistic UI. */
export function ulid(now: number = Date.now()): string {
  if (now === lastMs) counter++;
  else {
    lastMs = now;
    counter = 0;
  }
  let t = "";
  let ms = now;
  for (let i = 0; i < 10; i++) {
    t = CROCKFORD[ms % 32] + t;
    ms = Math.floor(ms / 32);
  }
  let r = "";
  const c = counter.toString(32).toUpperCase().padStart(4, "0");
  for (let i = 0; i < 12; i++) r += CROCKFORD[Math.floor(Math.random() * 32)];
  return (t + c + r).slice(0, 26);
}
