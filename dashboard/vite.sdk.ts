/** The SDK is a native NAPI addon: rolldown can't parse the `.node` binary, so every bundle leaves it external. */
export const PLOYZ_SDK = '@ployz/sdk'
export const PLOYZ_SDK_AND_SUBPATHS = /^@ployz\/sdk(?:\/|$)/
