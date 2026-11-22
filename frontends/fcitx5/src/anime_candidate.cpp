#include "anime_candidate.h"

#include <fcitx/text.h>

#include <algorithm>
#include <string>
#include <utility>

namespace anime {

AnimeCandidateWord::AnimeCandidateWord(const ipc::Candidate &cand,
                                       size_t index,
                                       bool show_comment,
                                       CandidateSelectCallback on_select)
    : index_(index), on_select_(std::move(on_select)) {
    std::string display = cand.text;
    if (cand.kind == ipc::CandidateKind::Continuation &&
        !display.starts_with("\xE2\x9C\xA6")) {
        display = "\xE2\x9C\xA6 " + display;
    }
    setText(fcitx::Text(std::move(display)));

    const auto comment = formatCandidateComment(cand, show_comment);
    if (!comment.empty()) {
        setComment(fcitx::Text(std::string(comment)));
    }
}

void AnimeCandidateWord::select(fcitx::InputContext *inputContext) const {
    if (on_select_) {
        on_select_(inputContext, index_);
    }
}

std::unique_ptr<fcitx::CommonCandidateList> buildCandidateList(
    const std::vector<ipc::Candidate> &candidates,
    const CandidatePresentationOptions &options,
    CandidateSelectCallback on_select) {
    const auto plan = decideCandidatePresentation(candidates, options);
    auto list = std::make_unique<fcitx::CommonCandidateList>();
    list->setPageSize(static_cast<int>(plan.page_size));
    list->setLabels({"1. ", "2. ", "3. ", "4. ", "5. ", "6. ", "7. ", "8. ", "9. "});

    switch (plan.layout) {
        case ResolvedLayout::Default:
            list->setLayoutHint(fcitx::CandidateLayoutHint::NotSet);
            break;
        case ResolvedLayout::Horizontal:
            list->setLayoutHint(fcitx::CandidateLayoutHint::Horizontal);
            break;
        case ResolvedLayout::Vertical:
            list->setLayoutHint(fcitx::CandidateLayoutHint::Vertical);
            break;
    }

    const size_t max_cap = std::clamp(options.max_candidates, size_t{1}, size_t{9});
    const size_t count = std::min(candidates.size(), max_cap);
    for (size_t i = 0; i < count; ++i) {
        list->append<AnimeCandidateWord>(candidates[i], i, options.show_comments, on_select);
    }
    if (!list->empty()) {
        list->setGlobalCursorIndex(0);
    }
    return list;
}

}  // namespace anime
