if(CMAKE_HOST_SYSTEM_NAME STREQUAL "Darwin")
    add_custom_target(install-network-helper
        COMMAND sudo -- /bin/sh "${CLANKERD_SOURCE_DIR}/cmake/scripts/network-helper.sh" install
            "${CLANKERD_ARTIFACT_DIR}/darwin-arm64/clankerd-network"
            "${CLANKERD_SOURCE_DIR}/cmake/launchd/io.clankerd.network.plist"
        DEPENDS build USES_TERMINAL VERBATIM)
    foreach(action uninstall restart)
        add_custom_target(${action}-network-helper
            COMMAND sudo -- /bin/sh "${CLANKERD_SOURCE_DIR}/cmake/scripts/network-helper.sh" ${action}
            USES_TERMINAL VERBATIM)
    endforeach()
    add_custom_target(verify-network-helper
        COMMAND "${CMAKE_COMMAND}" -E env "CARGO_TARGET_DIR=${CLANKERD_CARGO_TARGET_DIR}"
            "${CLANKERD_CARGO_EXECUTABLE}" test --locked -p clankerd-network-ipc --test native
            -- --ignored --nocapture --test-threads=1
        WORKING_DIRECTORY "${CLANKERD_SOURCE_DIR}" USES_TERMINAL VERBATIM)
    add_custom_target(status-network-helper
        COMMAND launchctl print system/io.clankerd.network
        USES_TERMINAL VERBATIM)
else()
    foreach(action install uninstall restart status verify)
        add_custom_target(${action}-network-helper
            COMMAND "${CMAKE_COMMAND}" -E echo "Network LaunchDaemon management requires macOS"
            COMMAND "${CMAKE_COMMAND}" -E false USES_TERMINAL VERBATIM)
    endforeach()
endif()
