#include <iostream>
#include <string>
#include "processor.h"

int main(int argc, char* argv[]) {
    if (argc < 3) {
        std::cerr << "Usage: " << argv[0] << " <file> <command>" << std::endl;
        std::cerr << "Commands: lines, words, chars, freq, find:<pattern>" << std::endl;
        return 1;
    }

    std::string filepath = argv[1];
    std::string command = argv[2];

    TextProcessor proc(filepath);
    try {
        proc.load();
    } catch (const std::exception& e) {
        std::cerr << "Error: " << e.what() << std::endl;
        return 1;
    }

    if (command == "lines") {
        std::cout << "Lines: " << proc.lineCount() << std::endl;
    } else if (command == "words") {
        std::cout << "Words: " << proc.wordCount() << std::endl;
    } else if (command == "chars") {
        std::cout << "Characters: " << proc.charCount() << std::endl;
    } else if (command == "freq") {
        std::cout << "Most frequent word: " << proc.mostFrequentWord() << std::endl;
    } else if (command.rfind("find:", 0) == 0) {
        std::string pattern = command.substr(5);
        auto matches = proc.findLines(pattern);
        for (const auto& line : matches) {
            std::cout << line << std::endl;
        }
    } else {
        std::cerr << "Unknown command: " << command << std::endl;
        return 1;
    }

    return 0;
}
