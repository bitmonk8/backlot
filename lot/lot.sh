#!/bin/bash
/Users/jonasd/dev/lot/target/release/lot run -c unity-scanner.yaml -- ${UNITYPATH}/Contents/MacOS/Unity -projectPath $PROJECTPATH
 $*
