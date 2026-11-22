#ifndef ANIME_FCITX5_ANIME_CANDIDATE_H
#define ANIME_FCITX5_ANIME_CANDIDATE_H

#include "ipc_protocol.h"

#include <fcitx/candidatelist.h>

#include <cstddef>
#include <functional>
#include <memory>
#include <vector>

namespace anime {

using CandidateSelectCallback = std::function<void(fcitx::InputContext *ic, size_t index)>;

class AnimeCandidateWord final : public fcitx::CandidateWord {
public:
    AnimeCandidateWord(const ipc::Candidate &cand,
                       size_t index,
                       CandidateSelectCallback on_select);

    void select(fcitx::InputContext *inputContext) const override;

private:
    size_t index_;
    CandidateSelectCallback on_select_;
};

std::unique_ptr<fcitx::CommonCandidateList> buildCandidateList(
    const std::vector<ipc::Candidate> &candidates,
    CandidateSelectCallback on_select);

}  // namespace anime

#endif  // ANIME_FCITX5_ANIME_CANDIDATE_H
