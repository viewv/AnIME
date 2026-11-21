cmake_minimum_required(VERSION 3.16)
project(fcitx5-anime VERSION 0.1.0 LANGUAGES CXX)
include(GNUInstallDirs)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC OFF)

find_package(Fcitx5Core REQUIRED)

include(CheckCXXSourceCompiles)
set(CMAKE_REQUIRED_LIBRARIES Fcitx5::Core)
set(CMAKE_REQUIRED_FLAGS "-std=c++20")
check_cxx_source_compiles("
#include <fcitx/candidatelist.h>
struct W : fcitx::CandidateWord {
  W() : CandidateWord(fcitx::Text(\"a\")) { setComment(fcitx::Text(\"b\")); }
  void select(fcitx::InputContext *) const override {}
};
int main() { W w; return 0; }" ANIME_HAVE_CANDIDATE_COMMENT)
check_cxx_source_compiles("
#include <fcitx/addonfactory.h>
#ifndef FCITX_ADDON_FACTORY_V2
#error no v2
#endif
int main() { return 0; }" ANIME_HAVE_FACTORY_V2)
unset(CMAKE_REQUIRED_LIBRARIES)
unset(CMAKE_REQUIRED_FLAGS)

foreach(feat ANIME_HAVE_CANDIDATE_COMMENT ANIME_HAVE_FACTORY_V2)
  if(NOT ${feat})
    message(FATAL_ERROR "Fcitx5Core (${Fcitx5Core_VERSION}) 缺少 ${feat} 所需接口，请升级 Fcitx5。")
  endif()
endforeach()

add_library(anime MODULE src/anime_engine.cpp)
set_target_properties(anime PROPERTIES PREFIX "lib")
target_link_libraries(anime Fcitx5::Core)
target_compile_options(anime PRIVATE -Wall -Wextra)

install(TARGETS anime DESTINATION ${CMAKE_INSTALL_LIBDIR}/fcitx5)
install(FILES anime-addon.conf DESTINATION ${CMAKE_INSTALL_DATADIR}/fcitx5/addon RENAME anime.conf)
install(FILES anime-inputmethod.conf DESTINATION ${CMAKE_INSTALL_DATADIR}/fcitx5/inputmethod RENAME anime.conf)
