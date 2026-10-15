!macro NSIS_HOOK_POSTINSTALL
  CreateShortCut "$DESKTOP\Fets Anime.lnk" "$INSTDIR\Fets Anime.exe"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  Delete "$DESKTOP\Fets Anime.lnk"
  MessageBox MB_YESNO|MB_ICONQUESTION "Hapus juga database, pengaturan, thumbnail, dan cache Fets Anime?" IDYES FetsAnimeDeleteData
  Goto FetsAnimeKeepData
  FetsAnimeDeleteData:
    RMDir /r "$APPDATA\FetsAnime"
  FetsAnimeKeepData:
!macroend
