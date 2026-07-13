#pragma once

#include <string>
#include <vector>
#include <unordered_map>

class TextProcessor {
public:
    explicit TextProcessor(const std::string& filepath);
    void load();
    int lineCount() const;
    int wordCount() const;
    int charCount() const;
    std::string mostFrequentWord() const;
    std::vector<std::string> findLines(const std::string& pattern) const;

private:
    std::string filepath_;
    std::vector<std::string> lines_;
    bool loaded_;
};
