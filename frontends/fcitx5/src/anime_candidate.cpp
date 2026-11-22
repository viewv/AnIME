#include "anime_candidate.h"

#include <fcitx/text.h>

#include <string>
#include <utility>

namespace anime {

AnimeCandidateWord::AnimeCandidateWord(const ipc::Candidate &cand,
                                       size_t index,
                                       CandidateSelectCallback on_select)
    : index_(index), on_select_(std::move(on_select)) {
    std::string display = cand.text;
    if (cand.kind == ipc::CandidateKind::Continuation &&
        !display.starts_with("\xE2\x9C\xA6")) {
        display = "\xE2\x9C\xA6 " + display;
    }
    setText(fcitx::Text(std::move(display)));

    if (cand.comment && !cand.comment->empty()) {
        if (cand.kind == ipc::CandidateKind::Continuation &&
            *cand.comment == "\xE2\x9C\xA6") {
            setComment(fcitx::Text("next"));
        } else {
            setComment(fcitx::Text(*cand.comment));
        }
    }
}

void AnimeCandidateWord::select(fcitx::InputContext *inputContext) const {
    if (on_select_) {
        on_select_(inputContext, index_);
    }
}

std::unique_ptr<fcitx::CommonCandidateList> buildCandidateList(
    const std::vector<ipc::Candidate> &candidates,
    CandidateSelectCallback on_select) {
    auto list = std::make_unique<fcitx::CommonCandidateList>();
    list->setPageSize(9);
    list->setLabels({"1. ", "2. ", "3. ", "4. ", "5. ", "6. ", "7. ", "8. ", "9. "});
    list->setLayoutHint(fcitx::CandidateLayoutHint::NotSet);

    const size_t count = candidates.size() < 9 ? candidates.size() : 9;
    for (size_t i = 0; i < count; ++i) {
        list->append<AnimeCandidateWord>(candidates[i], i, on_select);
    }
    if (!list->empty()) {
        list->setGlobalCursorIndex(0);
    }
    return list;
}

}  // namespace anime
