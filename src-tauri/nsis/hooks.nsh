; 繪匣 PaintDeck 的 NSIS 掛鉤（tauri.conf.json 的 bundle > windows > nsis > installerHooks）。
; 解除安裝：刪程式檔之前問要不要連模型、ComfyUI、所有資料一起刪（src\uninstall.rs）。
; App 內更新（/UPDATE）、安靜模式（/P）、無人值守（/S）都不問、不刪。$LANGUAGE 讓視窗跟移除程式同一種語言。
!macro NSIS_HOOK_PREUNINSTALL
  ${If} $UpdateMode <> 1
  ${AndIf} $PassiveMode <> 1
  ${AndIfNot} ${Silent}
    ExecWait '"$INSTDIR\${MAINBINARYNAME}.exe" --uninstall-cleanup --lang $LANGUAGE'
  ${EndIf}
!macroend
