// このファイルは自動生成です。直接編集しないでください。
//
// Rust のコマンドに実際に渡した引数（called）・実際に返ってきた値（returned）・イベントで送られた値（emitted）の見本です。
// 画面側の型（Backend）と合っていることを、TypeScript のコンパイラ（npm run typecheck）が確かめます。
// 値は形だけを残してあります（数値は 0、文字列は "…"。"recording" のような列挙の値はそのまま）。
//
// Rust 側を変えて内容が変わったときは、次のコマンドで書き直します（src-tauri/src/ipc_tests/scenario.rs）:
//   KIROSCRIBE_UPDATE_IPC_CONTRACT=1 cargo test -p kiroscribe --features e2e ipc_contract

import type { Backend } from "../lib/backend";

type Method = keyof Backend;
/** メソッドの引数（＝コマンドに渡す値。backend.ts が同じ名前のままコマンドに渡している） */
type Called<K extends Method> = Backend[K] extends (...args: infer A) => unknown ? A : never;
/** メソッドの戻り値（＝コマンドの戻り値。値を返さないコマンドは null が返る） */
type Returned<K extends Method> = Backend[K] extends (...args: never[]) => Promise<infer T>
  ? [T] extends [void]
    ? null
    : T
  : never;
/** イベントの購読で、コールバックに渡される値（＝イベントの内容） */
type Emitted<K extends Method> = Backend[K] extends (cb: (payload: infer T) => void) => unknown ? T : never;

export const called = {
  addSearchFolder: [
    [],
  ] satisfies Called<"addSearchFolder">[],
  answerRecordingCheck: [
    ["…",0,false],
    ["…",0,true],
  ] satisfies Called<"answerRecordingCheck">[],
  cancelAi: [
    ["…","enhance"],
  ] satisfies Called<"cancelAi">[],
  checkForUpdate: [
    [],
  ] satisfies Called<"checkForUpdate">[],
  clearChatMessages: [
    ["…"],
  ] satisfies Called<"clearChatMessages">[],
  closePopup: [
    [],
  ] satisfies Called<"closePopup">[],
  createNote: [
    [],
  ] satisfies Called<"createNote">[],
  deleteNote: [
    ["…"],
  ] satisfies Called<"deleteNote">[],
  enhanceNote: [
    ["…"],
  ] satisfies Called<"enhanceNote">[],
  exportMarkdown: [
    ["…",{"minutes":true,"notes":true,"transcript":true}],
  ] satisfies Called<"exportMarkdown">[],
  getApiKeyStatus: [
    [],
  ] satisfies Called<"getApiKeyStatus">[],
  getAppVersion: [
    [],
  ] satisfies Called<"getAppVersion">[],
  getDebugLogStatus: [
    [],
  ] satisfies Called<"getDebugLogStatus">[],
  getGuide: [
    [],
  ] satisfies Called<"getGuide">[],
  getNote: [
    ["…"],
  ] satisfies Called<"getNote">[],
  getPopupState: [
    [],
  ] satisfies Called<"getPopupState">[],
  getRecordingStatus: [
    [],
  ] satisfies Called<"getRecordingStatus">[],
  getReleaseNotes: [
    [],
  ] satisfies Called<"getReleaseNotes">[],
  getSearchIndexStatus: [
    [],
  ] satisfies Called<"getSearchIndexStatus">[],
  getSettings: [
    [],
  ] satisfies Called<"getSettings">[],
  getUpdateStatus: [
    [],
  ] satisfies Called<"getUpdateStatus">[],
  installUpdate: [
    [],
  ] satisfies Called<"installUpdate">[],
  listAudioDevices: [
    [],
  ] satisfies Called<"listAudioDevices">[],
  listChatMessages: [
    ["…"],
  ] satisfies Called<"listChatMessages">[],
  listNotes: [
    [],
  ] satisfies Called<"listNotes">[],
  listSegments: [
    ["…"],
  ] satisfies Called<"listSegments">[],
  listTranscriptBreaks: [
    ["…"],
  ] satisfies Called<"listTranscriptBreaks">[],
  openExternal: [
    ["…"],
  ] satisfies Called<"openExternal">[],
  openNoteFromPopup: [
    ["…",true],
  ] satisfies Called<"openNoteFromPopup">[],
  openSettingsFromPopup: [
    [],
  ] satisfies Called<"openSettingsFromPopup">[],
  pauseRecording: [
    [],
  ] satisfies Called<"pauseRecording">[],
  rebuildSearchIndex: [
    [],
  ] satisfies Called<"rebuildSearchIndex">[],
  removeSearchFolder: [
    ["…"],
  ] satisfies Called<"removeSearchFolder">[],
  resumeRecording: [
    [],
  ] satisfies Called<"resumeRecording">[],
  saveSettings: [
    [{"autoRecordMeetings":true,"autoUpdate":false,"captureSystemAudio":true,"claudeModel":"…","companion":false,"companionModel":"…","inputDeviceId":null,"language":"ja","launchAtLogin":true,"myName":"…","transcriptionModel":"…"}],
    [{"autoRecordMeetings":true,"autoUpdate":false,"captureSystemAudio":true,"claudeModel":"…","companion":true,"companionModel":"…","inputDeviceId":null,"language":"ja","launchAtLogin":true,"myName":"…","transcriptionModel":"…"}],
  ] satisfies Called<"saveSettings">[],
  searchNotes: [
    ["…"],
  ] satisfies Called<"searchNotes">[],
  sendChatMessage: [
    ["…","…"],
  ] satisfies Called<"sendChatMessage">[],
  setApiKey: [
    ["anthropic","…"],
    ["openai","…"],
  ] satisfies Called<"setApiKey">[],
  setDebugVerbose: [
    [false],
    [true],
  ] satisfies Called<"setDebugVerbose">[],
  setSearchFolderNoContent: [
    ["…",true],
  ] satisfies Called<"setSearchFolderNoContent">[],
  startRecording: [
    ["…"],
  ] satisfies Called<"startRecording">[],
  stopRecording: [
    [],
  ] satisfies Called<"stopRecording">[],
  submitDebugLogs: [
    ["contract"],
  ] satisfies Called<"submitDebugLogs">[],
  takeUpdateNotice: [
    [],
  ] satisfies Called<"takeUpdateNotice">[],
  updateNote: [
    ["…",{"title":"…","userNotes":"…"}],
  ] satisfies Called<"updateNote">[],
  writeDebugLog: [
    ["error","…",{"command":"x","message":"y"}],
  ] satisfies Called<"writeDebugLog">[],
};

export const returned = {
  addSearchFolder: [
    null,
    {"done":0,"files":0,"folders":[{"files":0,"missing":false,"noContent":false,"path":"…","truncated":false,"unconfirmed":false}],"revision":0,"state":"off","total":0,"updatedAt":null},
    {"done":0,"files":0,"folders":[{"files":0,"missing":false,"noContent":false,"path":"…","truncated":false,"unconfirmed":false}],"revision":0,"state":"ready","total":0,"updatedAt":0},
    {"done":0,"files":0,"folders":[{"files":0,"missing":false,"noContent":true,"path":"…","truncated":false,"unconfirmed":false}],"revision":0,"state":"building","total":0,"updatedAt":0},
  ] satisfies Returned<"addSearchFolder">[],
  answerRecordingCheck: [
    null,
  ] satisfies Returned<"answerRecordingCheck">[],
  cancelAi: [
    null,
  ] satisfies Returned<"cancelAi">[],
  checkForUpdate: [
    {"available":{"notes":"…","publishedAt":0,"version":"…"},"checkedAt":0,"downloadedBytes":0,"phase":"idle","ready":false,"totalBytes":null},
  ] satisfies Returned<"checkForUpdate">[],
  clearChatMessages: [
    null,
  ] satisfies Returned<"clearChatMessages">[],
  closePopup: [
    null,
  ] satisfies Returned<"closePopup">[],
  createNote: [
    {"createdAt":0,"enhancedAt":null,"enhancedNotes":null,"id":"…","title":"…","updatedAt":0,"userNotes":"…"},
  ] satisfies Returned<"createNote">[],
  deleteNote: [
    null,
  ] satisfies Returned<"deleteNote">[],
  enhanceNote: [
    {"createdAt":0,"enhancedAt":0,"enhancedNotes":"…","id":"…","title":"…","updatedAt":0,"userNotes":"…"},
  ] satisfies Returned<"enhanceNote">[],
  exportMarkdown: [
    {"fileName":"…","markdown":"…"},
  ] satisfies Returned<"exportMarkdown">[],
  getApiKeyStatus: [
    {"anthropic":false,"openai":false,"storage":"…"},
  ] satisfies Returned<"getApiKeyStatus">[],
  getAppVersion: [
    "…",
  ] satisfies Returned<"getAppVersion">[],
  getDebugLogStatus: [
    {"verboseUntil":null},
  ] satisfies Returned<"getDebugLogStatus">[],
  getGuide: [
    {"fetchedAt":0,"markdown":"…","source":"network"},
  ] satisfies Returned<"getGuide">[],
  getNote: [
    {"createdAt":0,"enhancedAt":null,"enhancedNotes":null,"id":"…","title":"…","updatedAt":0,"userNotes":"…"},
  ] satisfies Returned<"getNote">[],
  getPopupState: [
    null,
    {"app":"…","deadline":0,"kind":"confirmEnd","noteId":"…","title":"…"},
    {"app":"…","kind":"error","message":"…"},
    {"app":"…","kind":"recording","noteId":"…","title":"…"},
    {"app":"…","kind":"stopped","noteId":"…","title":"…"},
    {"app":"…","kind":"stoppedUnanswered","noteId":"…","title":"…"},
  ] satisfies Returned<"getPopupState">[],
  getRecordingStatus: [
    {"elapsedMs":0,"noteId":"…","runningSince":0,"sources":["me","them"],"state":"recording"},
    {"elapsedMs":0,"noteId":"…","runningSince":null,"sources":["me","them"],"state":"paused"},
    {"elapsedMs":0,"noteId":null,"runningSince":null,"sources":[],"state":"idle"},
  ] satisfies Returned<"getRecordingStatus">[],
  getReleaseNotes: [
    {"fetchedAt":0,"releases":[{"body":"…","htmlUrl":"…","name":"…","publishedAt":"…","tagName":"…","version":"…"}],"source":"network"},
  ] satisfies Returned<"getReleaseNotes">[],
  getSearchIndexStatus: [
    {"done":0,"files":0,"folders":[],"revision":0,"state":"off","total":0,"updatedAt":null},
    {"done":0,"files":0,"folders":[{"files":0,"missing":false,"noContent":false,"path":"…","truncated":false,"unconfirmed":false}],"revision":0,"state":"ready","total":0,"updatedAt":0},
  ] satisfies Returned<"getSearchIndexStatus">[],
  getSettings: [
    {"autoRecordMeetings":true,"autoUpdate":false,"captureSystemAudio":true,"claudeModel":"…","companion":true,"companionModel":"…","inputDeviceId":null,"language":"ja","launchAtLogin":true,"myName":"…","transcriptionModel":"…"},
  ] satisfies Returned<"getSettings">[],
  getUpdateStatus: [
    {"available":null,"checkedAt":null,"downloadedBytes":0,"phase":"idle","ready":false,"totalBytes":null},
    {"available":{"notes":"…","publishedAt":0,"version":"…"},"checkedAt":0,"downloadedBytes":0,"phase":"idle","ready":false,"totalBytes":null},
  ] satisfies Returned<"getUpdateStatus">[],
  listAudioDevices: [
    [{"id":"…","isDefault":true,"name":"…"}],
  ] satisfies Returned<"listAudioDevices">[],
  listChatMessages: [
    [{"content":"…","createdAt":0,"id":0,"noteId":"…","role":"assistant"},{"content":"…","createdAt":0,"id":0,"noteId":"…","role":"user"}],
  ] satisfies Returned<"listChatMessages">[],
  listNotes: [
    [{"createdAt":0,"hasEnhanced":false,"id":"…","preview":"…","title":"…","updatedAt":0},{"createdAt":0,"hasEnhanced":true,"id":"…","preview":"…","title":"…","updatedAt":0}],
  ] satisfies Returned<"listNotes">[],
  listSegments: [
    [{"endedAt":0,"id":0,"noteId":"…","speaker":"me","startedAt":0,"text":"…"},{"endedAt":0,"id":0,"noteId":"…","speaker":"them","startedAt":0,"text":"…"}],
  ] satisfies Returned<"listSegments">[],
  listTranscriptBreaks: [
    [{"id":0,"noteId":"…","round":0,"startedAt":0}],
  ] satisfies Returned<"listTranscriptBreaks">[],
  openExternal: [
    null,
  ] satisfies Returned<"openExternal">[],
  openNoteFromPopup: [
    null,
  ] satisfies Returned<"openNoteFromPopup">[],
  openSettingsFromPopup: [
    null,
  ] satisfies Returned<"openSettingsFromPopup">[],
  pauseRecording: [
    {"elapsedMs":0,"noteId":"…","runningSince":null,"sources":["me","them"],"state":"paused"},
  ] satisfies Returned<"pauseRecording">[],
  rebuildSearchIndex: [
    {"done":0,"files":0,"folders":[{"files":0,"missing":false,"noContent":true,"path":"…","truncated":false,"unconfirmed":false}],"revision":0,"state":"building","total":0,"updatedAt":0},
  ] satisfies Returned<"rebuildSearchIndex">[],
  removeSearchFolder: [
    {"done":0,"files":0,"folders":[],"revision":0,"state":"building","total":0,"updatedAt":0},
  ] satisfies Returned<"removeSearchFolder">[],
  resumeRecording: [
    {"elapsedMs":0,"noteId":"…","runningSince":0,"sources":["me","them"],"state":"recording"},
  ] satisfies Returned<"resumeRecording">[],
  saveSettings: [
    {"autoRecordMeetings":true,"autoUpdate":false,"captureSystemAudio":true,"claudeModel":"…","companion":false,"companionModel":"…","inputDeviceId":null,"language":"ja","launchAtLogin":true,"myName":"…","transcriptionModel":"…"},
    {"autoRecordMeetings":true,"autoUpdate":false,"captureSystemAudio":true,"claudeModel":"…","companion":true,"companionModel":"…","inputDeviceId":null,"language":"ja","launchAtLogin":true,"myName":"…","transcriptionModel":"…"},
  ] satisfies Returned<"saveSettings">[],
  searchNotes: [
    ["…"],
  ] satisfies Returned<"searchNotes">[],
  sendChatMessage: [
    {"content":"…","createdAt":0,"id":0,"noteId":"…","role":"assistant"},
  ] satisfies Returned<"sendChatMessage">[],
  setApiKey: [
    {"anthropic":false,"openai":true,"storage":"…"},
    {"anthropic":true,"openai":true,"storage":"…"},
  ] satisfies Returned<"setApiKey">[],
  setDebugVerbose: [
    {"verboseUntil":0},
    {"verboseUntil":null},
  ] satisfies Returned<"setDebugVerbose">[],
  setSearchFolderNoContent: [
    {"done":0,"files":0,"folders":[{"files":0,"missing":false,"noContent":true,"path":"…","truncated":false,"unconfirmed":false}],"revision":0,"state":"ready","total":0,"updatedAt":0},
  ] satisfies Returned<"setSearchFolderNoContent">[],
  startRecording: [
    {"elapsedMs":0,"noteId":"…","runningSince":0,"sources":["me","them"],"state":"recording"},
  ] satisfies Returned<"startRecording">[],
  stopRecording: [
    {"elapsedMs":0,"noteId":null,"runningSince":null,"sources":[],"state":"idle"},
  ] satisfies Returned<"stopRecording">[],
  submitDebugLogs: [
    {"id":"…","receivedAt":"…"},
  ] satisfies Returned<"submitDebugLogs">[],
  takeUpdateNotice: [
    null,
  ] satisfies Returned<"takeUpdateNotice">[],
  updateNote: [
    {"createdAt":0,"enhancedAt":null,"enhancedNotes":null,"id":"…","title":"…","updatedAt":0,"userNotes":"…"},
  ] satisfies Returned<"updateNote">[],
  writeDebugLog: [
    null,
  ] satisfies Returned<"writeDebugLog">[],
};

export const emitted = {
  onChatDelta: [
    {"delta":"…","noteId":"…"},
  ] satisfies Emitted<"onChatDelta">[],
  onEnhanceDelta: [
    {"delta":"…","noteId":"…"},
  ] satisfies Emitted<"onEnhanceDelta">[],
  onLevel: [
    {"level":0,"speaker":"me"},
    {"level":0,"speaker":"them"},
  ] satisfies Emitted<"onLevel">[],
  onLogSubmitProgress: [
    {"requestId":"contract","sent":0,"total":0},
  ] satisfies Emitted<"onLogSubmitProgress">[],
  onNoteCreated: [
    {"createdAt":0,"enhancedAt":null,"enhancedNotes":null,"id":"…","title":"…","updatedAt":0,"userNotes":"…"},
  ] satisfies Emitted<"onNoteCreated">[],
  onOpenNote: [
    {"enhance":true,"noteId":"…"},
  ] satisfies Emitted<"onOpenNote">[],
  onPopupUpdate: [
    {"app":"…","deadline":0,"kind":"confirmEnd","noteId":"…","title":"…"},
    {"app":"…","kind":"error","message":"…"},
    {"app":"…","kind":"finishing","noteId":"…","title":"…"},
    {"app":"…","kind":"recording","noteId":"…","title":"…"},
    {"app":"…","kind":"stopped","noteId":"…","title":"…"},
    {"app":"…","kind":"stoppedUnanswered","noteId":"…","title":"…"},
  ] satisfies Emitted<"onPopupUpdate">[],
  onRecordingError: [
    {"message":"…","noteId":"…"},
  ] satisfies Emitted<"onRecordingError">[],
  onRecordingStatus: [
    {"elapsedMs":0,"noteId":"…","runningSince":0,"sources":["me","them"],"state":"recording"},
    {"elapsedMs":0,"noteId":"…","runningSince":null,"sources":["me","them"],"state":"paused"},
    {"elapsedMs":0,"noteId":"…","runningSince":null,"sources":[],"state":"finishing"},
    {"elapsedMs":0,"noteId":null,"runningSince":null,"sources":[],"state":"idle"},
  ] satisfies Emitted<"onRecordingStatus">[],
  onSearchIndexStatus: [
    {"done":0,"files":0,"folders":[],"revision":0,"state":"building","total":0,"updatedAt":0},
    {"done":0,"files":0,"folders":[],"revision":0,"state":"building","total":0,"updatedAt":null},
    {"done":0,"files":0,"folders":[],"revision":0,"state":"erasing","total":0,"updatedAt":null},
    {"done":0,"files":0,"folders":[],"revision":0,"state":"off","total":0,"updatedAt":null},
    {"done":0,"files":0,"folders":[],"revision":0,"state":"ready","total":0,"updatedAt":0},
    {"done":0,"files":0,"folders":[{"files":0,"missing":false,"noContent":false,"path":"…","truncated":false,"unconfirmed":false}],"revision":0,"state":"building","total":0,"updatedAt":null},
    {"done":0,"files":0,"folders":[{"files":0,"missing":false,"noContent":false,"path":"…","truncated":false,"unconfirmed":false}],"revision":0,"state":"erasing","total":0,"updatedAt":null},
    {"done":0,"files":0,"folders":[{"files":0,"missing":false,"noContent":false,"path":"…","truncated":false,"unconfirmed":false}],"revision":0,"state":"off","total":0,"updatedAt":null},
    {"done":0,"files":0,"folders":[{"files":0,"missing":false,"noContent":false,"path":"…","truncated":false,"unconfirmed":false}],"revision":0,"state":"ready","total":0,"updatedAt":0},
    {"done":0,"files":0,"folders":[{"files":0,"missing":false,"noContent":true,"path":"…","truncated":false,"unconfirmed":false}],"revision":0,"state":"building","total":0,"updatedAt":0},
    {"done":0,"files":0,"folders":[{"files":0,"missing":false,"noContent":true,"path":"…","truncated":false,"unconfirmed":false}],"revision":0,"state":"ready","total":0,"updatedAt":0},
    {"done":0,"files":0,"folders":[{"files":0,"missing":false,"noContent":true,"path":"…","truncated":false,"unconfirmed":true}],"revision":0,"state":"building","total":0,"updatedAt":0},
    {"done":0,"files":0,"folders":[{"files":0,"missing":false,"noContent":true,"path":"…","truncated":false,"unconfirmed":true}],"revision":0,"state":"ready","total":0,"updatedAt":0},
  ] satisfies Emitted<"onSearchIndexStatus">[],
  onSegment: [
    {"endedAt":0,"id":0,"noteId":"…","speaker":"me","startedAt":0,"text":"…"},
    {"endedAt":0,"id":0,"noteId":"…","speaker":"them","startedAt":0,"text":"…"},
  ] satisfies Emitted<"onSegment">[],
  onSettingsChanged: [
    {"autoRecordMeetings":false,"autoUpdate":false,"captureSystemAudio":true,"claudeModel":"…","companion":true,"companionModel":"…","inputDeviceId":null,"language":"ja","launchAtLogin":true,"myName":"…","transcriptionModel":"…"},
    {"autoRecordMeetings":true,"autoUpdate":false,"captureSystemAudio":true,"claudeModel":"…","companion":true,"companionModel":"…","inputDeviceId":null,"language":"ja","launchAtLogin":true,"myName":"…","transcriptionModel":"…"},
  ] satisfies Emitted<"onSettingsChanged">[],
  onTranscriptBreak: [
    {"id":0,"noteId":"…","round":0,"startedAt":0},
  ] satisfies Emitted<"onTranscriptBreak">[],
  onUpdateStatus: [
    {"available":{"notes":"…","publishedAt":0,"version":"…"},"checkedAt":0,"downloadedBytes":0,"phase":"downloading","ready":false,"totalBytes":0},
    {"available":{"notes":"…","publishedAt":0,"version":"…"},"checkedAt":0,"downloadedBytes":0,"phase":"downloading","ready":false,"totalBytes":null},
    {"available":{"notes":"…","publishedAt":0,"version":"…"},"checkedAt":0,"downloadedBytes":0,"phase":"idle","ready":false,"totalBytes":null},
  ] satisfies Emitted<"onUpdateStatus">[],
};
