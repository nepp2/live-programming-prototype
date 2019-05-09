# Installing LLVM on Windows

Based on these instructions: https://llvm.org/docs/GettingStartedVS.html

## Summary

* Download the source code
* Point CMake GUI at the source
* Click "config", which should prompt a little window to pop up:
  * Choose Win64 from the drop-down
  * Add `host=x64` as an option (the instructions linked say `-Thost=x64`, but actually mean `-T host=x64`, and the `-T` is implicit in this dialog box)
* Leave the default options, except for:
  * change the installation directory from "program files" to something with no spaces, or everything will compile but cargo won't be able to build the bindings.
  * maybe include tools like Clang? (not sure about this)
* Click "generate". Should spit everything out into a target folder
* Open LLVM.sln. Check that it says "Win64".
* Change from "Debug" to "Release"
* Build the "INSTALL" project. I think this should run with no errors.
* Add the newly-installed `llvm/bin` folder to the user's `PATH` environment variable
