#!/bin/sh
# Create files and directories with unicode names, plus names carrying the
# characters SMB2 forbids on the wire. Also creates directories for the
# unicode-named shares defined in smb.conf (公開, café, 文档) so clients can
# exercise UTF-8 share-name enumeration too.
for dir in /shares/kokai /shares/cafe /shares/wendang; do
    mkdir -p "$dir"
    printf "content inside unicode-named share\n" > "$dir/README.txt"
done
chmod -R 777 /shares/kokai /shares/cafe /shares/wendang

BASE="/shares/public"
mkdir -p "$BASE"

# CJK characters
printf "Japanese test content\n" > "$BASE/日本語テスト.txt"
printf "Chinese test content\n" > "$BASE/中文测试.txt"

# Emoji directory and file
mkdir -p "$BASE/📁 folder"
printf "File inside emoji folder\n" > "$BASE/📁 folder/📄 document.txt"

# Accented characters
printf "French cafe content\n" > "$BASE/café.txt"
printf "German umlaut content\n" > "$BASE/Ärger.txt"
printf "Spanish content\n" > "$BASE/señor.txt"

# Cyrillic
printf "Russian document\n" > "$BASE/документ.txt"
printf "Ukrainian text\n" > "$BASE/привіт.txt"

# Mixed script directory
mkdir -p "$BASE/données"
printf "Mixed content\n" > "$BASE/données/résumé.txt"

# Arabic
printf "Arabic text\n" > "$BASE/مستند.txt"

# Characters SMB2 itself forbids in a name, stored the way every SMB client
# that carries POSIX names stores them: substituted into the Unicode
# private-use area at U+F000. These are the exact bytes macOS smbfs writes
# (read off a QNAP TS-464 over SSH, 2026-08-05), so an app that lists this
# directory and gets `who?.txt` back is byte-for-byte compatible with Finder.
#
# ❌ Don't replace the octal escapes with the literal characters. Samba would
# then hold a different name and the whole point of the fixture is gone.
#
#   \357\200\240 U+F020 "   \357\200\244 U+F024 >   \357\200\250 U+F028 trailing space
#   \357\200\241 U+F021 *   \357\200\245 U+F025 ?   \357\200\251 U+F029 trailing period
#   \357\200\242 U+F022 :   \357\200\246 U+F026 backslash
#   \357\200\243 U+F023 <   \357\200\247 U+F027 |
printf "question mark\n"   > "$BASE/$(printf 'who\357\200\245.txt')"
printf "double quote\n"    > "$BASE/$(printf '\357\200\240quoted\357\200\240.txt')"
printf "star\n"            > "$BASE/$(printf 'wild\357\200\241card.txt')"
printf "colon\n"           > "$BASE/$(printf '12\357\200\24230.txt')"
printf "angle brackets\n"  > "$BASE/$(printf '\357\200\243tag\357\200\244.txt')"
printf "backslash\n"       > "$BASE/$(printf 'back\357\200\246slash.txt')"
printf "pipe\n"            > "$BASE/$(printf 'a\357\200\247b.txt')"
printf "trailing space\n"  > "$BASE/$(printf 'trailing space\357\200\250')"
printf "trailing period\n" > "$BASE/$(printf 'trailing period\357\200\251')"
printf "all at once\n"     > "$BASE/$(printf '\357\200\240how_are_you_feeling\357\200\245\357\200\240_emojis.json')"

# Illegal characters in a directory component too, so a path has to be mapped
# per component rather than as one string.
mkdir -p "$BASE/$(printf 'we\357\200\245ird dir')"
printf "nested\n" > "$BASE/$(printf 'we\357\200\245ird dir')/$(printf 'in\357\200\241ner.txt')"

chmod -R 777 "$BASE"
