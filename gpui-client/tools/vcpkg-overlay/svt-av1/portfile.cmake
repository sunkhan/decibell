vcpkg_from_github(
    OUT_SOURCE_PATH SOURCE_PATH
    REPO AOMediaCodec/SVT-AV1
    REF "v${VERSION}"
    SHA512 3d3be6a290a4ec6899c35c9ec294d0429081b9e76a3872136671d5d61862f75fa43f48b53564f5e83bde3c25a0a100d7844adc2d9efb746b54b1cfcbd68cdcd3
    PATCHES
        no-force-llvm.diff
        no-inline-yy_unpacklo_epi128.diff
        no-safestringlib.diff
        unvendor-fastfeat.diff
)

file(REMOVE_RECURSE "${SOURCE_PATH}/third_party/aom/inc/")
file(REMOVE_RECURSE "${SOURCE_PATH}/third_party/aom_dsp/")
file(REMOVE_RECURSE "${SOURCE_PATH}/third_party/fastfeat/")
file(REMOVE_RECURSE "${SOURCE_PATH}/third_party/googletest/")
file(REMOVE_RECURSE "${SOURCE_PATH}/third_party/safestringlib/")

if (VCPKG_TARGET_ARCHITECTURE MATCHES "^(x86|x64)")
    vcpkg_find_acquire_program(NASM)
    set(SIMD_OPTIONS -DCOMPILE_C_ONLY=OFF "-DCMAKE_ASM_NASM_COMPILER=${NASM}")
else()
    if(VCPKG_TARGET_ARCHITECTURE MATCHES "^(arm64|arm64ec)$" AND NOT VCPKG_TARGET_IS_WINDOWS)
        set(SIMD_OPTIONS -DCOMPILE_C_ONLY=OFF)
    else()
        set(SIMD_OPTIONS -DCOMPILE_C_ONLY=ON)
    endif()
endif()

vcpkg_cmake_configure(
    SOURCE_PATH "${SOURCE_PATH}"
    OPTIONS
        ${SIMD_OPTIONS}
        -DBUILD_APPS=OFF
        -DREPRODUCIBLE_BUILDS=ON
        -DEXCLUDE_HASH=OFF
        -DBUILD_TESTING=OFF
        -DSVT_AV1_LTO=OFF
        "-DFASTFEAT_INCLUDE_DIR=${CURRENT_INSTALLED_DIR}/include/"
        OPTIONS_RELEASE
        "-DFASTFEAT_LIB_DIR=${CURRENT_INSTALLED_DIR}/lib/"
        "-DCMAKE_OUTPUT_DIRECTORY=${CURRENT_BUILDTREES_DIR}/${TARGET_TRIPLET}-rel/Bin/Release"
    OPTIONS_DEBUG
        "-DFASTFEAT_LIB_DIR=${CURRENT_INSTALLED_DIR}/debug/lib/"
        "-DCMAKE_OUTPUT_DIRECTORY=${CURRENT_BUILDTREES_DIR}/${TARGET_TRIPLET}-dbg/Bin/Debug"
)

vcpkg_cmake_install()
vcpkg_cmake_config_fixup(PACKAGE_NAME SVT-AV1 CONFIG_PATH lib/cmake/SVT-AV1)
vcpkg_copy_pdbs()
vcpkg_fixup_pkgconfig()

if("tool" IN_LIST FEATURES)
    vcpkg_copy_tools(TOOL_NAMES SvtAv1EncApp AUTO_CLEAN)
endif()

file(REMOVE_RECURSE "${CURRENT_PACKAGES_DIR}/debug/include")

vcpkg_install_copyright(FILE_LIST "${SOURCE_PATH}/LICENSE.md" "${SOURCE_PATH}/PATENTS.md")
