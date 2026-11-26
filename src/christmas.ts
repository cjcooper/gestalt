// Christmas, as in Paddock, Hot Mess and Subduer: it's on from the moment this
// build is installed, whatever the date, and a later release takes it out.
// styles.css keys its touches off data-christmas: lights along the toolbar, a
// Santa hat on the window title and snow over the welcome panel. Only the
// welcome panel's countdown follows the calendar.

/** Nights left until Christmas Day, counted from November 1; null on and after the day, and earlier in the year. */
export function sleepsUntilChristmas(today: Date): number | null {
  const month = today.getMonth(); // 0-based: 10 is November, 11 December
  if (month < 10 || (month === 11 && today.getDate() >= 25)) return null;
  const day = Date.UTC(today.getFullYear(), month, today.getDate());
  return Math.round((Date.UTC(today.getFullYear(), 11, 25) - day) / 86_400_000);
}

/** The welcome panel's line for `today`. */
export function christmasGreeting(today: Date): string {
  if (today.getMonth() === 11 && today.getDate() === 25) return "🎄 Merry Christmas!";
  const sleeps = sleepsUntilChristmas(today);
  if (sleeps === 1) return "🎄 One more sleep until Christmas!";
  if (sleeps !== null) return `🎄 ${sleeps} sleeps until Christmas`;
  return "🎄 Happy holidays from Gestalt";
}

/** Dresses the page up: the root's data-christmas, and the welcome panel's greeting if it has one. */
export function initChristmas(today: Date = new Date()): void {
  document.documentElement.setAttribute("data-christmas", "");
  const greeting = document.getElementById("welcome-greeting");
  if (greeting) greeting.textContent = christmasGreeting(today);
}
