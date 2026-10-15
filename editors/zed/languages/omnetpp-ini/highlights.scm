"[" @punctuation.bracket
"]" @punctuation.bracket
(section_heading "General" @keyword)
(section_heading type: _ @keyword)
(keyword_name) @keyword
(setting_name) @property
"=" @operator
(string_literal) @string
(comment) @comment

; functions
(xml_function "xml" @function.builtin)
(xml_function "(" @punctuation.bracket)
(xml_function ")" @punctuation.bracket)

(section_word) @type 
((string_literal) @boolean
 (#match? @boolean "^(true|false|yes|no)$"))

((string_literal) @number
 (#match? @number "^-?[0-9]+(\\.[0-9]+)?([eE][+-]?[0-9]+)?\\s*(s|ms|us|ns|ps|d|h|m|bps|kbps|Mbps|Gbps|B|KiB|MiB|Hz|kHz|MHz|GHz|W|mW)?$"))

((string_literal) @constant.builtin
 (#match? @constant.builtin "\\*\\*"))
