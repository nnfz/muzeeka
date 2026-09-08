let remaining = $state(false);
try { remaining = localStorage.getItem('muzeeka:remaining-time') === '1'; } catch { /* unavailable during SSR */ }
export const timeDisplay = {
  get remaining() { return remaining; },
  toggle() {
    remaining = !remaining;
    try { localStorage.setItem('muzeeka:remaining-time', remaining ? '1' : '0'); } catch { /* optional persistence */ }
  },
};
