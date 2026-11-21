!macro NSIS_HOOK_POSTINSTALL
  IfSilent portable_done
  IfFileExists "$INSTDIR\Singulo portable data\*.*" portable_done
  MessageBox MB_YESNO|MB_ICONQUESTION "Use portable mode? Choose Yes to keep Singulo settings beside the app on this disk. You can also enable it later in Settings." IDNO portable_done
  ClearErrors
  CreateDirectory "$INSTDIR\Singulo portable data"
  IfErrors 0 portable_done
  MessageBox MB_OK|MB_ICONEXCLAMATION "Could not create the portable settings folder. You can try enabling portable mode later in Settings."
  portable_done:
!macroend
