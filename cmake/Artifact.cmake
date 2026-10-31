set(artifact_config "${CMAKE_BINARY_DIR}/ArtifactConfig.cmake")
set(config "")
foreach(variable Python3_EXECUTABLE CLANKERD_SOURCE_DIR CLANKERD_WORK_DIR CLANKERD_DOWNLOAD_DIR CLANKERD_ARTIFACT_DIR CLANKERD_INPUTS CLANKERD_SIGNER CLANKERD_HOST_BINARY_DIR CLANKERD_GUEST_BINARY CLANKERD_MANAGEMENT_DIR CLANKERD_PACKAGE_DIRS LICENSE_INPUTS INPUTS)
    string(APPEND config "set(${variable} [==[${${variable}}]==])\n")
endforeach()
foreach(input IN LISTS LICENSE_INPUTS)
    string(APPEND config "set(${input}_ARCHIVE [==[${${input}_ARCHIVE}]==])\n")
endforeach()
file(GENERATE OUTPUT "${artifact_config}" CONTENT "${config}")
add_custom_target(build
    COMMAND "${CMAKE_COMMAND}" "-DCONFIG=${artifact_config}" -P "${CLANKERD_SOURCE_DIR}/cmake/Publish.cmake"
    DEPENDS cargo-host cargo-guest management ${CLANKERD_ARCHIVES}
    USES_TERMINAL VERBATIM)
if(CMAKE_HOST_SYSTEM_NAME STREQUAL "Darwin")
    add_custom_target(artifact ALL DEPENDS build)
endif()
