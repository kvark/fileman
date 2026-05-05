use egui::{Color32, FontId, TextFormat};
use syntect::util::LinesWithEndings;

use crate::theme::ThemeKind;

pub fn highlight_cmake_job(text: &str, theme_kind: ThemeKind) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let font_id = FontId::monospace(13.0);
    let (keyword_color, string_color, variable_color, comment_color, text_color, func_color) =
        match theme_kind {
            ThemeKind::Dark => (
                Color32::from_rgb(110, 179, 255),
                Color32::from_rgb(143, 219, 173),
                Color32::from_rgb(255, 206, 129),
                Color32::from_rgb(120, 132, 150),
                Color32::from_rgb(220, 225, 232),
                Color32::from_rgb(171, 162, 255),
            ),
            ThemeKind::Light => (
                Color32::from_rgb(25, 86, 178),
                Color32::from_rgb(20, 128, 92),
                Color32::from_rgb(170, 110, 20),
                Color32::from_rgb(120, 120, 120),
                Color32::from_rgb(40, 45, 55),
                Color32::from_rgb(110, 90, 190),
            ),
        };

    let keywords = [
        "if",
        "elseif",
        "else",
        "endif",
        "foreach",
        "endforeach",
        "while",
        "endwhile",
        "function",
        "endfunction",
        "macro",
        "endmacro",
        "return",
        "break",
        "continue",
        "block",
        "endblock",
    ];

    let builtin_commands = [
        "cmake_minimum_required",
        "project",
        "add_executable",
        "add_library",
        "add_subdirectory",
        "add_custom_command",
        "add_custom_target",
        "add_dependencies",
        "add_compile_definitions",
        "add_compile_options",
        "add_link_options",
        "target_link_libraries",
        "target_include_directories",
        "target_compile_definitions",
        "target_compile_options",
        "target_compile_features",
        "target_sources",
        "target_link_options",
        "target_link_directories",
        "target_precompile_headers",
        "set",
        "unset",
        "set_property",
        "get_property",
        "set_target_properties",
        "get_target_property",
        "option",
        "find_package",
        "find_library",
        "find_path",
        "find_file",
        "find_program",
        "include",
        "include_directories",
        "link_directories",
        "link_libraries",
        "install",
        "message",
        "string",
        "list",
        "file",
        "math",
        "configure_file",
        "execute_process",
        "cmake_parse_arguments",
        "get_filename_component",
        "get_cmake_property",
        "mark_as_advanced",
        "separate_arguments",
        "cmake_path",
        "cmake_policy",
        "enable_testing",
        "add_test",
        "set_tests_properties",
        "fetchcontent_declare",
        "fetchcontent_makeavailable",
        "fetchcontent_populate",
        "fetchcontent_getproperties",
    ];

    let constants = [
        "TRUE",
        "FALSE",
        "ON",
        "OFF",
        "YES",
        "NO",
        "AND",
        "OR",
        "NOT",
        "STREQUAL",
        "STRLESS",
        "STRGREATER",
        "EQUAL",
        "LESS",
        "GREATER",
        "MATCHES",
        "VERSION_EQUAL",
        "VERSION_LESS",
        "VERSION_GREATER",
        "DEFINED",
        "EXISTS",
        "IS_DIRECTORY",
        "IS_ABSOLUTE",
        "COMMAND",
        "TARGET",
        "IN_LIST",
        "IN",
        "LISTS",
        "ITEMS",
        "RANGE",
        "PUBLIC",
        "PRIVATE",
        "INTERFACE",
        "SHARED",
        "STATIC",
        "MODULE",
        "OBJECT",
        "IMPORTED",
        "ALIAS",
        "REQUIRED",
        "COMPONENTS",
        "CONFIG",
        "QUIET",
        "DESTINATION",
        "TARGETS",
        "FILES",
        "DIRECTORY",
        "RUNTIME",
        "LIBRARY",
        "ARCHIVE",
        "CACHE",
        "INTERNAL",
        "FORCE",
        "PARENT_SCOPE",
        "GLOB",
        "GLOB_RECURSE",
        "APPEND",
        "PREPEND",
        "REMOVE_ITEM",
        "REMOVE_DUPLICATES",
        "SORT",
        "FILTER",
        "FATAL_ERROR",
        "SEND_ERROR",
        "WARNING",
        "STATUS",
        "VERBOSE",
        "DEBUG",
        "DEPENDS",
        "WORKING_DIRECTORY",
        "COMMENT",
        "OUTPUT",
        "COMMAND",
    ];

    for line in LinesWithEndings::from(text) {
        let mut i = 0;
        let bytes = line.as_bytes();

        while i < bytes.len() {
            // Comments
            if bytes[i] == b'#' {
                append(&mut job, &line[i..], comment_color, font_id.clone());
                i = bytes.len();
                continue;
            }

            // Strings
            if bytes[i] == b'"' {
                let start = i;
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1; // closing quote
                }
                append(&mut job, &line[start..i], string_color, font_id.clone());
                continue;
            }

            // Variable references ${...}
            if i + 1 < bytes.len() && bytes[i] == b'$' && bytes[i + 1] == b'{' {
                let start = i;
                i += 2;
                while i < bytes.len() && bytes[i] != b'}' {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1;
                }
                append(&mut job, &line[start..i], variable_color, font_id.clone());
                continue;
            }

            // Generator expressions $<...>
            if i + 1 < bytes.len() && bytes[i] == b'$' && bytes[i + 1] == b'<' {
                let start = i;
                let mut depth = 0;
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'<' {
                        depth += 1;
                    } else if bytes[i] == b'>' {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    }
                    i += 1;
                }
                append(&mut job, &line[start..i], variable_color, font_id.clone());
                continue;
            }

            // Words (identifiers, keywords, commands)
            if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let word = &line[start..i];
                let word_lower = word.to_ascii_lowercase();
                let color = if keywords.contains(&word_lower.as_str()) {
                    keyword_color
                } else if builtin_commands.contains(&word_lower.as_str()) {
                    func_color
                } else if constants.contains(&word) {
                    keyword_color
                } else {
                    text_color
                };
                append(&mut job, word, color, font_id.clone());
                continue;
            }

            // Everything else
            let start = i;
            i += 1;
            while i < bytes.len()
                && bytes[i] != b'#'
                && bytes[i] != b'"'
                && bytes[i] != b'$'
                && !bytes[i].is_ascii_alphabetic()
                && bytes[i] != b'_'
            {
                i += 1;
            }
            append(&mut job, &line[start..i], text_color, font_id.clone());
        }
    }
    job
}

fn append(job: &mut egui::text::LayoutJob, text: &str, color: Color32, font_id: FontId) {
    job.append(
        text,
        0.0,
        TextFormat {
            font_id,
            color,
            ..Default::default()
        },
    );
}
