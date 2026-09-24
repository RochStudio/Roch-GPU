using System.Runtime.InteropServices;

// Load vendor DLLs from the installed driver, not beside the elevated app or in its working folder.
[assembly: DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
