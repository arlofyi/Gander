// SPDX-License-Identifier: Apache-2.0
//! `Content-Type` and the listing `mime` field, by extension only (the host never sniffs file
//! contents for this).

/// The MIME type of a lower-case extension, if known.
pub fn for_ext(ext: &str) -> Option<&'static str> {
    Some(match ext {
        // Documents
        "pdf" => "application/pdf",
        "txt" | "log" | "ini" | "cfg" | "conf" => "text/plain",
        "md" | "markdown" => "text/markdown",
        "csv" => "text/csv",
        "tsv" => "text/tab-separated-values",
        "rtf" => "application/rtf",
        "json" => "application/json",
        "xml" => "application/xml",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "yaml" | "yml" => "application/yaml",
        "toml" => "application/toml",
        "doc" => "application/msword",
        "dot" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "docm" => "application/vnd.ms-word.document.macroEnabled.12",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "xlsm" => "application/vnd.ms-excel.sheet.macroEnabled.12",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "pptm" => "application/vnd.ms-powerpoint.presentation.macroEnabled.12",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "odp" => "application/vnd.oasis.opendocument.presentation",
        "vsdx" => "application/vnd.ms-visio.drawing",
        "epub" => "application/epub+zip",
        // Images
        "jpg" | "jpeg" | "jpe" | "jfif" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "bmp" | "dib" => "image/bmp",
        "webp" => "image/webp",
        "tif" | "tiff" => "image/tiff",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "avif" => "image/avif",
        "jxl" => "image/jxl",
        "ico" => "image/vnd.microsoft.icon",
        "svg" => "image/svg+xml",
        "psd" => "image/vnd.adobe.photoshop",
        "dng" => "image/x-adobe-dng",
        "cr2" => "image/x-canon-cr2",
        "cr3" => "image/x-canon-cr3",
        "nef" => "image/x-nikon-nef",
        "arw" => "image/x-sony-arw",
        "raf" => "image/x-fuji-raf",
        "orf" => "image/x-olympus-orf",
        "rw2" => "image/x-panasonic-rw2",
        // Audio and video
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "wav" => "audio/wav",
        "wma" => "audio/x-ms-wma",
        "mp4" | "m4v" => "video/mp4",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "webm" => "video/webm",
        "wmv" => "video/x-ms-wmv",
        // Archives
        "zip" => "application/zip",
        "7z" => "application/x-7z-compressed",
        "rar" => "application/vnd.rar",
        "tar" => "application/x-tar",
        "gz" => "application/gzip",
        _ => return None,
    })
}

/// The `Content-Type` sent by `/file`: the known type, else `application/octet-stream`.
pub fn content_type(ext: Option<&str>) -> &'static str {
    ext.and_then(for_ext).unwrap_or("application/octet-stream")
}

#[cfg(test)]
mod tests {
    #[test]
    fn known_and_unknown() {
        assert_eq!(
            super::for_ext("pptx"),
            Some("application/vnd.openxmlformats-officedocument.presentationml.presentation")
        );
        assert_eq!(super::content_type(None), "application/octet-stream");
        assert_eq!(super::content_type(Some("exe")), "application/octet-stream");
    }
}
