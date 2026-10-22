; "Open with WaveFlow" for the audio formats WaveFlow plays, without taking
; any of them over.
;
; Tauri's own file associations (`bundle.fileAssociations`) write each
; extension's default class, which makes the installer the default player
; for every format it lists. They are declared for macOS only
; (`tauri.macos.conf.json`), and Windows gets these hooks instead, which only
; offer WaveFlow:
;
; - the `WaveFlow.Audio` ProgID, with its icon and its open command;
; - `.ext\OpenWithProgids`, which lists it under "Open with" for each format;
; - `Applications\<exe>\SupportedTypes`, which keeps WaveFlow out of the
;   "Open with" list of every other type;
; - `Capabilities` + `RegisteredApplications`, which list WaveFlow in
;   Settings > Default apps, where the user can make it the default for a
;   format if they want to.
;
; Everything goes under SHCTX, the hive the template's `SetContext` picked
; for this install (per user or per machine). The MSI does the same through
; `open-with.wxs`. Keep the formats in step with tauri.macos.conf.json, the
; Linux desktop template and the Flatpak desktop entry.
;
; The template includes this file before it defines MAINBINARYNAME and
; MANUPRODUCTKEY, and a `!define` expands what it names when it is read: the
; paths built from them live in the macro bodies, which expand where they
; are inserted, once both exist.

!define WAVEFLOW_PROGID "WaveFlow.Audio"

!macro WAVEFLOW_FOR_EACH_FORMAT MACRO
  !insertmacro ${MACRO} "mp3"
  !insertmacro ${MACRO} "flac"
  !insertmacro ${MACRO} "wav"
  !insertmacro ${MACRO} "aiff"
  !insertmacro ${MACRO} "aif"
  !insertmacro ${MACRO} "ogg"
  !insertmacro ${MACRO} "oga"
  !insertmacro ${MACRO} "opus"
  !insertmacro ${MACRO} "m4a"
  !insertmacro ${MACRO} "aac"
  !insertmacro ${MACRO} "dsf"
  !insertmacro ${MACRO} "dff"
!macroend

!macro WAVEFLOW_OFFER_FORMAT EXT
  WriteRegStr SHCTX "Software\Classes\.${EXT}\OpenWithProgids" "${WAVEFLOW_PROGID}" ""
  WriteRegStr SHCTX "Software\Classes\Applications\${MAINBINARYNAME}.exe\SupportedTypes" ".${EXT}" ""
  WriteRegStr SHCTX "${MANUPRODUCTKEY}\Capabilities\FileAssociations" ".${EXT}" "${WAVEFLOW_PROGID}"
!macroend

; Only our own value. The extension key, its `OpenWithProgids` and the
; default they may name belong to the other players too, and `/ifempty`
; would not protect them: it looks at subkeys, not at values.
!macro WAVEFLOW_WITHDRAW_FORMAT EXT
  DeleteRegValue SHCTX "Software\Classes\.${EXT}\OpenWithProgids" "${WAVEFLOW_PROGID}"
!macroend

!macro NSIS_HOOK_POSTINSTALL
  WriteRegStr SHCTX "Software\Classes\${WAVEFLOW_PROGID}" "" "${PRODUCTNAME} audio file"
  WriteRegStr SHCTX "Software\Classes\${WAVEFLOW_PROGID}\DefaultIcon" "" "$\"$INSTDIR\${MAINBINARYNAME}.exe$\",0"
  WriteRegStr SHCTX "Software\Classes\${WAVEFLOW_PROGID}\shell\open\command" "" "$\"$INSTDIR\${MAINBINARYNAME}.exe$\" $\"%1$\""

  WriteRegStr SHCTX "Software\Classes\Applications\${MAINBINARYNAME}.exe" "FriendlyAppName" "${PRODUCTNAME}"
  WriteRegStr SHCTX "Software\Classes\Applications\${MAINBINARYNAME}.exe\DefaultIcon" "" "$\"$INSTDIR\${MAINBINARYNAME}.exe$\",0"
  WriteRegStr SHCTX "Software\Classes\Applications\${MAINBINARYNAME}.exe\shell\open\command" "" "$\"$INSTDIR\${MAINBINARYNAME}.exe$\" $\"%1$\""

  WriteRegStr SHCTX "${MANUPRODUCTKEY}\Capabilities" "ApplicationName" "${PRODUCTNAME}"
  WriteRegStr SHCTX "${MANUPRODUCTKEY}\Capabilities" "ApplicationDescription" "Local-first music player"
  WriteRegStr SHCTX "Software\RegisteredApplications" "${PRODUCTNAME}" "${MANUPRODUCTKEY}\Capabilities"

  !insertmacro WAVEFLOW_FOR_EACH_FORMAT WAVEFLOW_OFFER_FORMAT

  ; SHCNE_ASSOCCHANGED: Explorer rereads the associations now rather than
  ; at its next restart.
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; An update runs the old uninstaller with /UPDATE before installing the
  ; new version, whose post-install writes all of this again: leave it be,
  ; as the template leaves the shortcuts.
  ${If} $UpdateMode <> 1
    !insertmacro WAVEFLOW_FOR_EACH_FORMAT WAVEFLOW_WITHDRAW_FORMAT

    DeleteRegValue SHCTX "Software\RegisteredApplications" "${PRODUCTNAME}"
    DeleteRegKey SHCTX "${MANUPRODUCTKEY}\Capabilities"
    DeleteRegKey SHCTX "Software\Classes\Applications\${MAINBINARYNAME}.exe"
    DeleteRegKey SHCTX "Software\Classes\${WAVEFLOW_PROGID}"

    System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
  ${EndIf}
!macroend
