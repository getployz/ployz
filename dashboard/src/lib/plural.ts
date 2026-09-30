/** "1 change", "3 changes". */
export const plural = (n: number, noun: string) => `${n} ${noun}${n === 1 ? "" : "s"}`;

const names = new Intl.ListFormat("en-GB", { type: "conjunction" });
/** "a", "a and b", "a, b and c". */
export const listNames = (list: string[]) => names.format(list);
