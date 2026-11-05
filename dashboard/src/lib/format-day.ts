/** A calendar day, e.g. "Nov 5, 2026". Fixed to en-US and UTC so the server render and the browser agree on the text. */
const dayFormat = new Intl.DateTimeFormat("en-US", { dateStyle: "medium", timeZone: "UTC" });

export function formatDay(date: Date | string) {
  return dayFormat.format(new Date(date));
}
