/// The community server's caps on names, in UTF-8 bytes (src/community
/// db.hpp and the SERVER_UPDATE handler). Inputs clamp to them as the user
/// types (lib/utf8 clampLine), so a save comes back exactly as typed.
export const CHANNEL_NAME_MAX_BYTES = 64;
export const ROLE_NAME_MAX_BYTES = 64;
export const NICKNAME_MAX_BYTES = 32;
export const SERVER_NAME_MAX_BYTES = 64;
export const SERVER_DESCRIPTION_MAX_BYTES = 512;
