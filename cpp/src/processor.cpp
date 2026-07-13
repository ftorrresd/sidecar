#include "processor.h"
#include <fstream>
#include <sstream>
#include <algorithm>
#include <stdexcept>

TextProcessor::TextProcessor(const std::string& filepath)
    : filepath_(filepath), loaded_(false) {}

void TextProcessor::load() {
    std::ifstream file(filepath_);
    if (!file.is_open()) {
        throw std::runtime_error("Cannot open file: " + filepath_);
    }

    std::string line;
    while (std::getline(file, line)) {
        lines_.push_back(line);
    }
    loaded_ = true;
}

int TextProcessor::lineCount() const {
    return static_cast<int>(lines_.size());
}

int TextProcessor::wordCount() const {
    int count = 0;
    for (const auto& line : lines_) {
        std::istringstream iss(line);
        std::string word;
        while (iss >> word) {
            count++;
        }
    }
    return count;
}

int TextProcessor::charCount() const {
    int count = 0;
    for (const auto& line : lines_) {
        count += static_cast<int>(line.length());
    }
    return count;
}

std::string TextProcessor::mostFrequentWord() const {
    std::unordered_map<std::string, int> freq;
    for (const auto& line : lines_) {
        std::istringstream iss(line);
        std::string word;
        while (iss >> word) {
            freq[word]++;
        }
    }

    std::string maxWord;
    int maxCount = 0;
    for (const auto& [word, count] : freq) {
        if (count > maxCount) {
            maxCount = count;
            maxWord = word;
        }
    }
    return maxWord;
}

std::vector<std::string> TextProcessor::findLines(const std::string& pattern) const {
    std::vector<std::string> result;
    for (const auto& line : lines_) {
        if (line.find(pattern) != std::string::npos) {
            result.push_back(line);
        }
    }
    return result;
}
