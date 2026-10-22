import { randomInt } from "node:crypto";

const ADJECTIVES = [
  "amber", "ashen", "azure", "bold", "brave", "bright", "brisk", "calm", "clever", "copper",
  "coral", "crimson", "dusky", "eager", "early", "fleet", "gentle", "gilded", "golden", "hardy",
  "hazel", "honest", "ivory", "jade", "keen", "lively", "lucky", "lunar", "mellow", "misty",
  "noble", "olive", "pale", "plucky", "proud", "quick", "quiet", "rapid", "rosy", "russet",
  "rustic", "sandy", "silver", "sleek", "snowy", "solar", "steady", "stormy", "sunny", "swift",
  "tawny", "tidy", "velvet", "vivid", "warm", "wild", "windy", "wise", "witty", "zesty",
];

const BIRDS = [
  "albatross", "avocet", "bittern", "bunting", "buzzard", "condor", "coot", "crane", "curlew", "dipper",
  "dove", "dunlin", "eagle", "egret", "falcon", "finch", "flamingo", "gannet", "goshawk", "grebe",
  "grouse", "harrier", "heron", "hobby", "ibis", "jackdaw", "jay", "kestrel", "kingfisher", "kite",
  "lapwing", "lark", "linnet", "magpie", "martin", "merlin", "nightjar", "osprey", "owl", "pelican",
  "petrel", "pipit", "plover", "puffin", "quail", "raven", "redstart", "robin", "rook", "sandpiper",
  "shrike", "siskin", "skylark", "sparrow", "starling", "swallow", "tern", "thrush", "wagtail", "wren",
];

const pick = (words: readonly string[]) => words[randomInt(words.length)];

/** A two-word name such as `amber-falcon`, for a Fleet Token made without one. */
export function generateTokenName() {
  return `${pick(ADJECTIVES)}-${pick(BIRDS)}`;
}

/**
 * The Machine Name a hostname becomes, by ployz-core's `MachineName::from_hostname`: each run of characters a DNS label
 * refuses turned into one `-`, lowercased, cut to 63 characters and trimmed of `-`. Null when nothing usable is left.
 */
export function machineNameFromHostname(hostname: string) {
  const label = hostname.replace(/[^A-Za-z0-9]+/gu, "-").toLowerCase().replace(/^-/u, "").slice(0, 63).replace(/-$/u, "");
  return label === "" ? null : label;
}
