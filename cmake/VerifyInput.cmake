file(SHA256 "${ARCHIVE}" actual)
if(NOT actual STREQUAL SHA256)
    message(FATAL_ERROR "Cached input failed SHA256 verification: ${ARCHIVE}")
endif()
