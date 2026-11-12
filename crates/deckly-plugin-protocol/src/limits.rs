//! Every bound of the protocol. The host and the SDK share these numbers:
//! the SDK never sends more, and the host treats more as a protocol error.

/// Largest body of a JSON message frame.
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
/// Largest body of a binary blob frame (one image).
pub const MAX_BLOB_BYTES: usize = 4 * 1024 * 1024;
/// Largest frame body of any kind; a larger length is refused before
/// anything is allocated.
pub const MAX_FRAME_BYTES: usize = if MAX_BLOB_BYTES > MAX_MESSAGE_BYTES {
    MAX_BLOB_BYTES
} else {
    MAX_MESSAGE_BYTES
};

/// Plugin and action ids: reverse-DNS names.
pub const MAX_ID_BYTES: usize = 128;
/// The id the host gives one placed key.
pub const MAX_CONTEXT_BYTES: usize = 256;
/// Device ids, platform and session names, versions, language tags.
pub const MAX_SHORT_TEXT_BYTES: usize = 128;
/// Names, authors, licences, tooltips, device names.
pub const MAX_NAME_CHARS: usize = 200;
/// A plugin's description.
pub const MAX_DESCRIPTION_CHARS: usize = 2000;
/// A key title.
pub const MAX_TITLE_CHARS: usize = 200;
/// One text line of a key face.
pub const MAX_FACE_TEXT_CHARS: usize = 32;
/// A path inside the plugin folder.
pub const MAX_PATH_BYTES: usize = 260;
/// A host service name.
pub const MAX_SERVICE_BYTES: usize = 64;

/// States of one action.
pub const MAX_STATES: usize = 16;
/// Actions of one plugin.
pub const MAX_ACTIONS: usize = 256;
/// Devices listed in `hello`.
pub const MAX_DEVICES: usize = 64;
/// Host services listed in `hello` or a manifest.
pub const MAX_SERVICES: usize = 64;
/// Programs (one per target) in a manifest.
pub const MAX_PROGRAMS: usize = 32;
/// Settings of one key, or a plugin's global settings, as JSON text.
pub const MAX_SETTINGS_BYTES: usize = 64 * 1024;
/// One settings form description, as JSON text.
pub const MAX_FORM_BYTES: usize = 64 * 1024;
/// Fields of one form, sections and notes included.
pub const MAX_FORM_FIELDS: usize = 64;
/// The key of a form field: the property it sets in the settings object.
pub const MAX_FIELD_KEY_BYTES: usize = 64;
/// A field label, a placeholder, a choice label.
pub const MAX_LABEL_CHARS: usize = 100;
/// A help line under a field, or the text of a note.
pub const MAX_HELP_CHARS: usize = 500;
/// Options of one choice, fixed or answered by the plugin.
pub const MAX_CHOICES: usize = 256;
/// The value of one choice option.
pub const MAX_CHOICE_VALUE_BYTES: usize = 256;
/// File name extensions a file field accepts.
pub const MAX_EXTENSIONS: usize = 16;
/// One file name extension, without the dot.
pub const MAX_EXTENSION_BYTES: usize = 16;
/// A field's default value, or the value a condition compares with, as
/// JSON text.
pub const MAX_DEFAULT_BYTES: usize = 4096;
/// The longest text a text or secret field may ask for.
pub const MAX_TEXT_VALUE_CHARS: usize = 8192;
/// The reason a plugin gives when it cannot list choices.
pub const MAX_ERROR_CHARS: usize = 200;
/// Largest side of a raw RGBA image.
pub const MAX_IMAGE_SIDE: u32 = 1024;
/// Largest `deckly-plugin.json`.
pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
