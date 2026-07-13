cmake_minimum_required(VERSION 3.14)
project(textprocessor VERSION 0.1.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

add_executable(textproc
    src/main.cpp
    src/processor.cpp
)

target_include_directories(textproc PRIVATE include)
