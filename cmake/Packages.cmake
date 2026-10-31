set(CLANKERD_ARCHIVES)
foreach(input IN LISTS INPUTS LICENSE_INPUTS)
    set(archive "${CLANKERD_DOWNLOAD_DIR}/${input}-${${input}_SHA256}.archive")
    set(${input}_ARCHIVE "${archive}")
    add_custom_command(OUTPUT "${archive}"
        COMMAND "${CMAKE_COMMAND}" "-DURL=${${input}_URL}" "-DSHA256=${${input}_SHA256}" "-DOUTPUT=${archive}" -P "${CLANKERD_SOURCE_DIR}/cmake/Download.cmake"
        DEPENDS "${CLANKERD_SOURCE_DIR}/cmake/Download.cmake" "${CLANKERD_INPUTS}"
        VERBATIM)
    list(APPEND CLANKERD_ARCHIVES "${archive}")
endforeach()
add_custom_target(verify-inputs)
foreach(input IN LISTS INPUTS LICENSE_INPUTS)
    add_custom_command(TARGET verify-inputs POST_BUILD
        COMMAND "${CMAKE_COMMAND}" "-DURL=${${input}_URL}" "-DSHA256=${${input}_SHA256}" "-DOUTPUT=${${input}_ARCHIVE}" -P "${CLANKERD_SOURCE_DIR}/cmake/Download.cmake" VERBATIM)
endforeach()
set(CLANKERD_PACKAGE_STAMPS)
set(CLANKERD_PACKAGE_DIRS)
foreach(input IN LISTS GUEST_INPUTS)
    set(directory "${CLANKERD_WORK_DIR}/packages/${input}")
    add_custom_command(OUTPUT "${directory}/.stamp"
        COMMAND "${CMAKE_COMMAND}" "-DARCHIVE=${${input}_ARCHIVE}" "-DSHA256=${${input}_SHA256}" -P "${CLANKERD_SOURCE_DIR}/cmake/VerifyInput.cmake"
        COMMAND "${CMAKE_COMMAND}" -E rm -rf "${directory}"
        COMMAND "${Python3_EXECUTABLE}" "${CLANKERD_SOURCE_DIR}/cmake/scripts/extract-binary.py" "${${input}_ARCHIVE}" "${directory}"
        COMMAND "${CMAKE_COMMAND}" -E touch "${directory}/.stamp"
        DEPENDS "${${input}_ARCHIVE}" "${CLANKERD_SOURCE_DIR}/cmake/scripts/extract-binary.py" "${CLANKERD_SOURCE_DIR}/cmake/VerifyInput.cmake"
        VERBATIM)
    list(APPEND CLANKERD_PACKAGE_STAMPS "${directory}/.stamp")
    list(APPEND CLANKERD_PACKAGE_DIRS "${directory}")
endforeach()
file(GLOB_RECURSE management_files CONFIGURE_DEPENDS "${CLANKERD_SOURCE_DIR}/crates/clankerd-guestd/rootfs/*")
file(GENERATE OUTPUT "${CMAKE_BINARY_DIR}/ManagementFiles.txt" CONTENT "${management_files}\n")
set(CLANKERD_MANAGEMENT_DIR "${CLANKERD_WORK_DIR}/management")
add_custom_command(OUTPUT "${CLANKERD_MANAGEMENT_DIR}/.stamp"
    COMMAND "${CMAKE_COMMAND}" -E rm -rf "${CLANKERD_MANAGEMENT_DIR}"
    COMMAND "${Python3_EXECUTABLE}" "${CLANKERD_SOURCE_DIR}/cmake/scripts/assemble-management.py" "${CLANKERD_MANAGEMENT_DIR}" "${CLANKERD_SOURCE_DIR}/crates/clankerd-guestd/rootfs" ${CLANKERD_PACKAGE_DIRS} --required ${GUEST_REQUIRED}
    COMMAND "${CMAKE_COMMAND}" -E touch "${CLANKERD_MANAGEMENT_DIR}/.stamp"
    DEPENDS ${CLANKERD_PACKAGE_STAMPS} ${management_files} "${CMAKE_BINARY_DIR}/ManagementFiles.txt" "${CLANKERD_INPUTS}" "${CLANKERD_SOURCE_DIR}/cmake/scripts/assemble-management.py"
    VERBATIM)
add_custom_target(management DEPENDS "${CLANKERD_MANAGEMENT_DIR}/.stamp")
